//! Real-time objects passed to `IAudioProcessor::process`: a fixed-capacity
//! `IEventList`, fixed-capacity `IParameterChanges`/`IParamValueQueue`, and
//! the plain → normalized parameter mapping. Nothing here allocates after
//! construction.
//!
//! # Interior mutability
//!
//! COM methods take `&self`, so the data lives in [`RtCell`]s. Host-side
//! mutation (`clear`/`push`) and plugin-side calls never overlap: the host
//! fills the lists, calls `process` (during which only the plugin touches
//! them, on the same thread), then reads or clears them. The owning
//! `Vst3Processor` is only ever used through `&mut self`, so no two threads
//! can be inside this protocol at once.

use std::cell::UnsafeCell;

use cadenza_dsp::MidiEvent;
use vst3::Steinberg::Vst::{
	Event, Event_, IEventList, IEventListTrait, IParamValueQueue, IParamValueQueueTrait,
	IParameterChanges, IParameterChangesTrait, NoteOffEvent, NoteOnEvent, ParamID, ParamValue,
};
use vst3::Steinberg::{int32, tresult};
use vst3::Steinberg::{kInvalidArgument, kResultFalse, kResultOk};
use vst3::{Class, ComWrapper};

use super::com::guard;

/// See the module docs for the access protocol that makes this sound.
pub(crate) struct RtCell<T>(UnsafeCell<T>);

// SAFETY: access is serialized by the process protocol described above.
unsafe impl<T: Send> Sync for RtCell<T> {}

impl<T> RtCell<T> {
	pub fn new(v: T) -> Self {
		Self(UnsafeCell::new(v))
	}

	#[allow(clippy::mut_from_ref)]
	fn get(&self) -> &mut T {
		// SAFETY: see the module docs; callers never hold two of these at once.
		unsafe { &mut *self.0.get() }
	}
}

// --------------------------------------------------------------- event list

/// Fixed-capacity `IEventList`. As an output list (capacity 0) it simply
/// rejects everything the plugin emits.
pub(crate) struct EventList {
	events: RtCell<Vec<Event>>,
}

impl Class for EventList {
	type Interfaces = (IEventList,);
}

fn zero_event() -> Event {
	// SAFETY: `Event` is a plain C struct/union; all-zero is a valid value.
	unsafe { std::mem::zeroed() }
}

impl EventList {
	pub fn with_capacity(capacity: usize) -> ComWrapper<EventList> {
		ComWrapper::new(EventList {
			events: RtCell::new(Vec::with_capacity(capacity)),
		})
	}

	pub fn clear(&self) {
		self.events.get().clear();
	}

	pub fn len(&self) -> usize {
		self.events.get().len()
	}

	fn push(&self, e: Event) -> bool {
		let events = self.events.get();
		if events.len() < events.capacity() {
			events.push(e);
			true
		} else {
			false
		}
	}

	pub fn push_note_on(&self, offset: u32, pitch: u8, velocity: f32) -> bool {
		let mut e = zero_event();
		e.sampleOffset = offset.min(i32::MAX as u32) as i32;
		e.flags = Event_::EventFlags_::kIsLive as u16;
		e.r#type = Event_::EventTypes_::kNoteOnEvent as u16;
		e.__field0.noteOn = NoteOnEvent {
			channel: 0,
			pitch: i16::from(pitch.min(127)),
			tuning: 0.0,
			velocity: velocity.clamp(0.0, 1.0),
			length: 0,
			noteId: -1,
		};
		self.push(e)
	}

	pub fn push_note_off(&self, offset: u32, pitch: u8) -> bool {
		let mut e = zero_event();
		e.sampleOffset = offset.min(i32::MAX as u32) as i32;
		e.flags = Event_::EventFlags_::kIsLive as u16;
		e.r#type = Event_::EventTypes_::kNoteOffEvent as u16;
		e.__field0.noteOff = NoteOffEvent {
			channel: 0,
			pitch: i16::from(pitch.min(127)),
			velocity: 0.0,
			noteId: -1,
			tuning: 0.0,
		};
		self.push(e)
	}

	#[cfg(test)]
	pub fn get(&self, i: usize) -> Option<Event> {
		self.events.get().get(i).copied()
	}
}

impl IEventListTrait for EventList {
	unsafe fn getEventCount(&self) -> int32 {
		guard(0, || self.len() as int32)
	}

	unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
		guard(kResultFalse, || {
			let events = self.events.get();
			match usize::try_from(index).ok().and_then(|i| events.get(i)) {
				Some(ev) if !e.is_null() => {
					// SAFETY: checked non-null out-parameter.
					unsafe { *e = *ev };
					kResultOk
				}
				_ => kInvalidArgument,
			}
		})
	}

	unsafe fn addEvent(&self, e: *mut Event) -> tresult {
		guard(kResultFalse, || {
			if e.is_null() {
				return kInvalidArgument;
			}
			// SAFETY: checked non-null.
			if self.push(unsafe { *e }) {
				kResultOk
			} else {
				kResultFalse
			}
		})
	}
}

/// Tracks sounding notes so "all notes off" (which VST3 events cannot
/// express) becomes one note-off per held key.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HeldNotes(u128);

impl HeldNotes {
	/// Converts one Cadenza event (offset clamped to `frames - 1`) into the list.
	pub fn push(&mut self, list: &EventList, e: &MidiEvent, frames: u32) {
		let time = e.offset().min(frames.saturating_sub(1));
		match *e {
			MidiEvent::NoteOn {
				pitch, velocity, ..
			} => {
				if list.push_note_on(time, pitch, velocity) {
					self.0 |= 1u128 << pitch.min(127);
				}
			}
			MidiEvent::NoteOff { pitch, .. } => {
				if list.push_note_off(time, pitch) {
					self.0 &= !(1u128 << pitch.min(127));
				}
			}
			MidiEvent::AllNotesOff { .. } => self.release_all(list, time),
		}
	}

	pub fn release_all(&mut self, list: &EventList, time: u32) {
		let mut held = self.0;
		while held != 0 {
			let pitch = held.trailing_zeros() as u8;
			held &= held - 1;
			if list.push_note_off(time, pitch) {
				self.0 &= !(1u128 << pitch);
			}
		}
	}
}

// -------------------------------------------------------- parameter changes

#[derive(Clone, Copy, Default)]
struct Point {
	id: ParamID,
	offset: i32,
	value: ParamValue,
	count: i32,
}

/// One parameter's changes for a block. Cadenza sends a single point (the
/// latest value, at offset 0); as an output queue it keeps the last point.
pub(crate) struct ParamValueQueue {
	point: RtCell<Point>,
}

impl Class for ParamValueQueue {
	type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for ParamValueQueue {
	unsafe fn getParameterId(&self) -> ParamID {
		guard(0, || self.point.get().id)
	}

	unsafe fn getPointCount(&self) -> int32 {
		guard(0, || self.point.get().count)
	}

	unsafe fn getPoint(
		&self,
		index: int32,
		sample_offset: *mut int32,
		value: *mut ParamValue,
	) -> tresult {
		guard(kResultFalse, || {
			let p = *self.point.get();
			if index != 0 || p.count == 0 || sample_offset.is_null() || value.is_null() {
				return kInvalidArgument;
			}
			// SAFETY: checked non-null out-parameters.
			unsafe {
				*sample_offset = p.offset;
				*value = p.value;
			}
			kResultOk
		})
	}

	unsafe fn addPoint(
		&self,
		sample_offset: int32,
		value: ParamValue,
		index: *mut int32,
	) -> tresult {
		guard(kResultFalse, || {
			let p = self.point.get();
			p.offset = sample_offset;
			p.value = value;
			p.count = 1;
			if !index.is_null() {
				// SAFETY: checked non-null out-parameter.
				unsafe { *index = 0 };
			}
			kResultOk
		})
	}
}

/// Fixed-capacity `IParameterChanges`: a pool of queues claimed in order.
pub(crate) struct ParameterChanges {
	queues: Vec<ComWrapper<ParamValueQueue>>,
	used: RtCell<usize>,
}

impl Class for ParameterChanges {
	type Interfaces = (IParameterChanges,);
}

impl ParameterChanges {
	pub fn with_capacity(capacity: usize) -> ComWrapper<ParameterChanges> {
		ComWrapper::new(ParameterChanges {
			queues: (0..capacity)
				.map(|_| {
					ComWrapper::new(ParamValueQueue {
						point: RtCell::new(Point::default()),
					})
				})
				.collect(),
			used: RtCell::new(0),
		})
	}

	pub fn clear(&self) {
		*self.used.get() = 0;
	}

	pub fn len(&self) -> usize {
		*self.used.get()
	}

	fn claim(&self, id: ParamID) -> Option<usize> {
		let used = self.used.get();
		if let Some(i) = (0..*used).find(|&i| self.queues[i].point.get().id == id) {
			return Some(i);
		}
		if *used >= self.queues.len() {
			return None;
		}
		let i = *used;
		*used += 1;
		*self.queues[i].point.get() = Point {
			id,
			..Point::default()
		};
		Some(i)
	}

	/// Sets `id` to the normalized `value` at offset 0 (latest value wins).
	pub fn push(&self, id: ParamID, value: ParamValue) -> bool {
		let Some(i) = self.claim(id) else {
			return false;
		};
		let p = self.queues[i].point.get();
		p.offset = 0;
		p.value = value;
		p.count = 1;
		true
	}

	fn queue_ptr(&self, i: usize) -> *mut IParamValueQueue {
		self.queues[i]
			.as_com_ref::<IParamValueQueue>()
			.map_or(std::ptr::null_mut(), |r| r.as_ptr())
	}
}

impl IParameterChangesTrait for ParameterChanges {
	unsafe fn getParameterCount(&self) -> int32 {
		guard(0, || self.len() as int32)
	}

	unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
		guard(std::ptr::null_mut(), || {
			match usize::try_from(index) {
				// Borrowed pointer (no add-ref), as the SDK's ParameterChanges.
				Ok(i) if i < self.len() => self.queue_ptr(i),
				_ => std::ptr::null_mut(),
			}
		})
	}

	unsafe fn addParameterData(
		&self,
		id: *const ParamID,
		index: *mut int32,
	) -> *mut IParamValueQueue {
		guard(std::ptr::null_mut(), || {
			if id.is_null() {
				return std::ptr::null_mut();
			}
			// SAFETY: checked non-null.
			match self.claim(unsafe { *id }) {
				Some(i) => {
					if !index.is_null() {
						// SAFETY: checked non-null out-parameter.
						unsafe { *index = i as int32 };
					}
					self.queue_ptr(i)
				}
				None => std::ptr::null_mut(),
			}
		})
	}
}

/// Opaque pointer helper: a borrowed interface pointer for `ProcessData`.
pub(crate) fn iface<C: Class, I: vst3::Interface>(obj: &ComWrapper<C>) -> *mut I {
	obj.as_com_ref::<I>()
		.map_or(std::ptr::null_mut(), |r| r.as_ptr())
}

// ---------------------------------------------------- plain → normalized

/// Samples per parameter of the `normalizedParamToPlain` curve.
pub(crate) const CURVE_POINTS: usize = 33;

/// Maps plain values (what `Processor::set_param` receives, in the ranges the
/// descriptor advertises) to VST3 normalized values on the audio thread,
/// where the controller (a main-thread object) cannot be asked.
///
/// For every parameter the controller's `normalizedParamToPlain` is sampled at
/// [`CURVE_POINTS`] evenly spaced normalized positions when the instance is
/// created; the inverse is then a binary search plus linear interpolation
/// over that monotonic curve. This is exact for linear parameters and at the
/// sample points, and very close for skewed (log/exponential) ones. Stepped
/// parameters map exactly: `(round(plain) - min) / (max - min)`, which is the
/// SDK's `normalized = step / stepCount`. Curves that are not monotonic fall
/// back to linear mapping between their end points.
///
/// Where exactness matters and blocking is allowed (flushing before a state
/// save), the host converts with the controller's `plainParamToNormalized`
/// on the plugin main thread instead.
pub(crate) struct ParamCurves {
	curves: Vec<f64>,
	stepped: Vec<bool>,
	monotonic: Vec<bool>,
}

impl ParamCurves {
	/// `sample(i, normalized)` must return the plain value of parameter `i`.
	pub fn build(
		count: usize,
		stepped: &[bool],
		mut sample: impl FnMut(usize, f64) -> f64,
	) -> Self {
		let mut curves = Vec::with_capacity(count * CURVE_POINTS);
		let mut monotonic = Vec::with_capacity(count);
		for i in 0..count {
			let start = curves.len();
			for k in 0..CURVE_POINTS {
				let v = sample(i, k as f64 / (CURVE_POINTS - 1) as f64);
				curves.push(if v.is_finite() { v } else { 0.0 });
			}
			let c = &curves[start..];
			let up = c.windows(2).all(|w| w[0] <= w[1]);
			let down = c.windows(2).all(|w| w[0] >= w[1]);
			monotonic.push((up || down) && c[0] != c[CURVE_POINTS - 1]);
		}
		Self {
			curves,
			stepped: (0..count)
				.map(|i| stepped.get(i).copied().unwrap_or(false))
				.collect(),
			monotonic,
		}
	}

	#[cfg(test)]
	pub fn len(&self) -> usize {
		self.monotonic.len()
	}

	/// Normalized value for plain `v` of parameter `i`, in `0..=1`.
	pub fn normalize(&self, i: usize, v: f64) -> f64 {
		let Some(c) = self.curves.get(i * CURVE_POINTS..(i + 1) * CURVE_POINTS) else {
			return 0.0;
		};
		let (first, last) = (c[0], c[CURVE_POINTS - 1]);
		let linear = |v: f64| {
			if last == first {
				0.0
			} else {
				((v - first) / (last - first)).clamp(0.0, 1.0)
			}
		};
		if self.stepped[i] {
			return linear(v.round());
		}
		if !self.monotonic[i] {
			return linear(v);
		}
		let increasing = first < last;
		// Index of the first sample at or past `v` along the curve direction.
		let j = c.partition_point(|&x| if increasing { x < v } else { x > v });
		if j == 0 {
			return 0.0;
		}
		if j >= CURVE_POINTS {
			return 1.0;
		}
		let (a, b) = (c[j - 1], c[j]);
		let frac = if b == a { 0.0 } else { (v - a) / (b - a) };
		((j - 1) as f64 + frac.clamp(0.0, 1.0)) / (CURVE_POINTS - 1) as f64
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use vst3::ComRef;

	#[test]
	fn event_list_is_bounded_and_readable() {
		let list = EventList::with_capacity(2);
		assert!(list.push_note_on(5, 60, 0.5));
		assert!(list.push_note_off(9, 60));
		assert!(!list.push_note_on(0, 61, 1.0));
		let r: ComRef<IEventList> = list.as_com_ref().unwrap();
		unsafe {
			assert_eq!(r.getEventCount(), 2);
			let mut e = zero_event();
			assert_eq!(r.getEvent(0, &mut e), kResultOk);
			assert_eq!(e.r#type, Event_::EventTypes_::kNoteOnEvent as u16);
			assert_eq!(e.sampleOffset, 5);
			assert_eq!(e.__field0.noteOn.pitch, 60);
			assert_eq!(e.__field0.noteOn.velocity, 0.5);
			assert_eq!(e.__field0.noteOn.noteId, -1);
			assert_eq!(r.getEvent(1, &mut e), kResultOk);
			assert_eq!(e.r#type, Event_::EventTypes_::kNoteOffEvent as u16);
			assert_eq!(r.getEvent(2, &mut e), kInvalidArgument);
			assert_eq!(r.addEvent(&mut e), kResultFalse);
		}
		list.clear();
		assert_eq!(list.len(), 0);
	}

	#[test]
	fn all_notes_off_releases_held_notes() {
		let list = EventList::with_capacity(16);
		let mut held = HeldNotes::default();
		for pitch in [60, 64, 127] {
			held.push(
				&list,
				&MidiEvent::NoteOn {
					offset: 0,
					pitch,
					velocity: 1.0,
				},
				32,
			);
		}
		held.push(
			&list,
			&MidiEvent::NoteOff {
				offset: 3,
				pitch: 64,
			},
			32,
		);
		held.push(&list, &MidiEvent::AllNotesOff { offset: 99 }, 32);
		assert_eq!(list.len(), 6);
		let offs: Vec<(i16, i32)> = (4..6)
			.map(|i| {
				let e = list.get(i).unwrap();
				assert_eq!(e.r#type, Event_::EventTypes_::kNoteOffEvent as u16);
				(unsafe { e.__field0.noteOff.pitch }, e.sampleOffset)
			})
			.collect();
		assert_eq!(offs, vec![(60, 31), (127, 31)]);
		list.clear();
		held.release_all(&list, 0);
		assert_eq!(list.len(), 0);
	}

	#[test]
	fn parameter_changes_one_point_per_param() {
		let changes = ParameterChanges::with_capacity(2);
		assert!(changes.push(7, 0.25));
		assert!(changes.push(7, 0.5));
		assert!(changes.push(9, 1.0));
		assert!(!changes.push(11, 0.0));
		let r: ComRef<IParameterChanges> = changes.as_com_ref().unwrap();
		unsafe {
			assert_eq!(r.getParameterCount(), 2);
			let q = ComRef::from_raw(r.getParameterData(0)).unwrap();
			assert_eq!(q.getParameterId(), 7);
			assert_eq!(q.getPointCount(), 1);
			let (mut off, mut v) = (-1, 0.0);
			assert_eq!(q.getPoint(0, &mut off, &mut v), kResultOk);
			assert_eq!((off, v), (0, 0.5));
			assert_eq!(q.getPoint(1, &mut off, &mut v), kInvalidArgument);
			assert!(r.getParameterData(2).is_null());
		}
		changes.clear();
		assert_eq!(changes.len(), 0);
		// Output use: the plugin adds data and points itself.
		unsafe {
			let mut index = -1;
			let q = ComRef::from_raw(r.addParameterData(&3, &mut index)).unwrap();
			assert_eq!(index, 0);
			q.addPoint(12, 0.75, &mut index);
			assert_eq!(r.getParameterCount(), 1);
			let again = r.addParameterData(&3, &mut index);
			assert_eq!(again, q.as_ptr());
		}
	}

	#[test]
	fn curves_invert_linear_log_and_stepped() {
		let stepped = [false, false, true, false];
		let curves = ParamCurves::build(4, &stepped, |i, n| match i {
			0 => -60.0 + 72.0 * n,
			1 => 20.0 * 1000f64.powf(n),
			2 => (n * 4.0).round(),
			_ => 5.0,
		});
		assert!((curves.normalize(0, 0.0) - 60.0 / 72.0).abs() < 1e-12);
		assert_eq!(curves.normalize(0, -100.0), 0.0);
		assert_eq!(curves.normalize(0, 100.0), 1.0);
		// Log curve: exact at sample points, close in between.
		assert!((curves.normalize(1, 20.0 * 1000f64.powf(0.5)) - 0.5).abs() < 1e-12);
		let mid = curves.normalize(1, 1000.0);
		let exact = (1000.0f64 / 20.0).ln() / 1000f64.ln();
		assert!((mid - exact).abs() < 0.01, "{mid} vs {exact}");
		assert_eq!(curves.normalize(2, 2.4), 0.5);
		assert_eq!(curves.normalize(3, 5.0), 0.0);
		let inverted = ParamCurves::build(1, &[false], |_, n| 1.0 - n);
		assert!((inverted.normalize(0, 0.25) - 0.75).abs() < 1e-12);
		assert_eq!(inverted.len(), 1);
	}
}
