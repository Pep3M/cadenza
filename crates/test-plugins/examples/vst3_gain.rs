//! Minimal VST3 gain plugin used to test `cadenza-plugin-host`'s VST3 host.
//!
//! Built as a `cdylib` example; the host tests wrap it in a `.vst3` bundle.
//! It deliberately uses a *separate* edit controller class connected through
//! `IConnectionPoint`, reports 32 samples of latency, and exposes:
//! - param 0 "Gain": -60..12 dB (normalized linearly), default 0 dB;
//! - param 1 "Mute": stepped switch (stepCount 1);
//! - param 2 "Meter": read-only, which hosts must not expose;
//! - param 3 "Program": hidden program-change parameter of the root unit, whose
//!   `IUnitInfo` program list "Factory" (id 1) holds "Unity" (0 dB), "Quiet"
//!   (-12 dB) and "Whisper" (-30 dB); selecting one sets the gain.
//!
//! Component state is the gain and mute values (normalized, f64 LE); the
//! controller state is the 4 bytes `CTRL`.
//!
//! Editor: `createView("editor")` returns a view that accepts
//! `X11EmbedWindowID` parents (it draws nothing). Once attached it registers a
//! 10 ms timer on the frame's `IRunLoop`; the first tick scripts a GUI edit
//! (`beginEdit(0)`, `performEdit(0, -6 dB)`, `endEdit(0)` on the component
//! handler), the second asks the frame to resize the view to 320x240.

#![allow(non_snake_case)]
// VST3 enum constants are `c_uint` on Unix but `c_int` on Windows.
#![allow(clippy::unnecessary_cast)]

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use vst3::Steinberg::Linux::{IRunLoop, IRunLoopTrait, ITimerHandler, ITimerHandlerTrait};
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, uid};

const LATENCY: u32 = 32;

/// Factory programs: name and gain (dB), selected through param 3.
const PROGRAMS: [(&str, f64); 3] = [("Unity", 0.0), ("Quiet", -12.0), ("Whisper", -30.0)];

/// The program a normalized program-change value selects.
fn program_at(value: f64) -> usize {
	((value.clamp(0.0, 1.0) * (PROGRAMS.len() - 1) as f64).round() as usize).min(PROGRAMS.len() - 1)
}

fn copy_cstring(src: &str, dst: &mut [c_char]) {
	for (d, s) in dst.iter_mut().zip(src.bytes().chain([0])) {
		*d = s as c_char;
	}
	if let Some(last) = dst.last_mut() {
		*last = 0;
	}
}

fn copy_wstring(src: &str, dst: &mut [TChar]) {
	let mut n = 0;
	for (d, s) in dst.iter_mut().zip(src.encode_utf16()) {
		*d = s;
		n += 1;
	}
	let end = n.min(dst.len() - 1);
	dst[end] = 0;
}

fn db_to_normalized(db: f64) -> f64 {
	((db + 60.0) / 72.0).clamp(0.0, 1.0)
}

fn normalized_to_db(n: f64) -> f64 {
	-60.0 + 72.0 * n.clamp(0.0, 1.0)
}

unsafe fn read_f64s<const N: usize>(stream: *mut IBStream) -> Option<[f64; N]> {
	let stream = unsafe { ComRef::from_raw(stream)? };
	let mut out = [0.0; N];
	for v in &mut out {
		let mut bytes = [0u8; 8];
		let mut read = 0;
		let r = unsafe { stream.read(bytes.as_mut_ptr().cast(), 8, &mut read) };
		if r != kResultOk || read != 8 {
			return None;
		}
		*v = f64::from_le_bytes(bytes);
	}
	Some(out)
}

unsafe fn write_bytes(stream: *mut IBStream, bytes: &[u8]) -> tresult {
	let Some(stream) = (unsafe { ComRef::from_raw(stream) }) else {
		return kInvalidArgument;
	};
	let mut written = 0;
	let mut data = bytes.to_vec();
	unsafe { stream.write(data.as_mut_ptr().cast(), data.len() as i32, &mut written) }
}

struct GainProcessor {
	gain: AtomicU64,
	mute: AtomicU64,
	connected: AtomicBool,
}

impl Class for GainProcessor {
	type Interfaces = (IComponent, IAudioProcessor, IConnectionPoint);
}

impl GainProcessor {
	const CID: TUID = uid(0x43444E5A, 0x56334741, 0x494E5052, 0x4F430001);
}

impl IPluginBaseTrait for GainProcessor {
	unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
		kResultOk
	}

	unsafe fn terminate(&self) -> tresult {
		kResultOk
	}
}

impl IConnectionPointTrait for GainProcessor {
	unsafe fn connect(&self, other: *mut IConnectionPoint) -> tresult {
		self.connected.store(!other.is_null(), Ordering::Relaxed);
		kResultOk
	}

	unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
		self.connected.store(false, Ordering::Relaxed);
		kResultOk
	}

	unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
		kResultOk
	}
}

impl IComponentTrait for GainProcessor {
	unsafe fn getControllerClassId(&self, class_id: *mut TUID) -> tresult {
		unsafe { *class_id = GainController::CID };
		kResultOk
	}

	unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
		kResultOk
	}

	unsafe fn getBusCount(&self, media_type: MediaType, _dir: BusDirection) -> i32 {
		if media_type == MediaTypes_::kAudio as MediaType {
			1
		} else {
			0
		}
	}

	unsafe fn getBusInfo(
		&self,
		media_type: MediaType,
		dir: BusDirection,
		index: i32,
		bus: *mut BusInfo,
	) -> tresult {
		if media_type != MediaTypes_::kAudio as MediaType || index != 0 {
			return kInvalidArgument;
		}
		let bus = unsafe { &mut *bus };
		bus.mediaType = media_type;
		bus.direction = dir;
		bus.channelCount = 2;
		copy_wstring(if dir == 0 { "Input" } else { "Output" }, &mut bus.name);
		bus.busType = BusTypes_::kMain as BusType;
		bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
		kResultOk
	}

	unsafe fn getRoutingInfo(&self, _in: *mut RoutingInfo, _out: *mut RoutingInfo) -> tresult {
		kNotImplemented
	}

	unsafe fn activateBus(&self, _t: MediaType, _d: BusDirection, _i: i32, _s: TBool) -> tresult {
		kResultOk
	}

	unsafe fn setActive(&self, _state: TBool) -> tresult {
		kResultOk
	}

	unsafe fn setState(&self, state: *mut IBStream) -> tresult {
		match unsafe { read_f64s::<2>(state) } {
			Some([gain, mute]) => {
				self.gain.store(gain.to_bits(), Ordering::Relaxed);
				self.mute.store(mute.to_bits(), Ordering::Relaxed);
				kResultOk
			}
			None => kResultFalse,
		}
	}

	unsafe fn getState(&self, state: *mut IBStream) -> tresult {
		let mut bytes = Vec::new();
		bytes.extend_from_slice(&f64::from_bits(self.gain.load(Ordering::Relaxed)).to_le_bytes());
		bytes.extend_from_slice(&f64::from_bits(self.mute.load(Ordering::Relaxed)).to_le_bytes());
		unsafe { write_bytes(state, &bytes) }
	}
}

impl IAudioProcessorTrait for GainProcessor {
	unsafe fn setBusArrangements(
		&self,
		inputs: *mut SpeakerArrangement,
		num_ins: i32,
		outputs: *mut SpeakerArrangement,
		num_outs: i32,
	) -> tresult {
		let ok = num_ins == 1
			&& num_outs == 1
			&& unsafe { *inputs == SpeakerArr::kStereo && *outputs == SpeakerArr::kStereo };
		if ok { kResultTrue } else { kResultFalse }
	}

	unsafe fn getBusArrangement(
		&self,
		_dir: BusDirection,
		index: i32,
		arr: *mut SpeakerArrangement,
	) -> tresult {
		if index != 0 {
			return kInvalidArgument;
		}
		unsafe { *arr = SpeakerArr::kStereo };
		kResultOk
	}

	unsafe fn canProcessSampleSize(&self, size: i32) -> tresult {
		if size == SymbolicSampleSizes_::kSample32 as i32 {
			kResultOk
		} else {
			kNotImplemented
		}
	}

	unsafe fn getLatencySamples(&self) -> u32 {
		LATENCY
	}

	unsafe fn setupProcessing(&self, _setup: *mut ProcessSetup) -> tresult {
		kResultOk
	}

	unsafe fn setProcessing(&self, _state: TBool) -> tresult {
		kResultOk
	}

	unsafe fn process(&self, data: *mut ProcessData) -> tresult {
		let data = unsafe { &*data };
		if let Some(changes) = unsafe { ComRef::from_raw(data.inputParameterChanges) } {
			for i in 0..unsafe { changes.getParameterCount() } {
				let Some(queue) = (unsafe { ComRef::from_raw(changes.getParameterData(i)) }) else {
					continue;
				};
				let count = unsafe { queue.getPointCount() };
				let (mut offset, mut value) = (0, 0.0);
				if count < 1
					|| unsafe { queue.getPoint(count - 1, &mut offset, &mut value) } != kResultOk
				{
					continue;
				}
				match unsafe { queue.getParameterId() } {
					0 => self.gain.store(value.to_bits(), Ordering::Relaxed),
					1 => self.mute.store(value.to_bits(), Ordering::Relaxed),
					3 => {
						let db = PROGRAMS[program_at(value)].1;
						self.gain
							.store(db_to_normalized(db).to_bits(), Ordering::Relaxed);
					}
					_ => {}
				}
			}
		}
		let n = data.numSamples as usize;
		if n == 0 || data.numInputs != 1 || data.numOutputs != 1 {
			return kResultOk;
		}
		let muted = f64::from_bits(self.mute.load(Ordering::Relaxed)) >= 0.5;
		let db = normalized_to_db(f64::from_bits(self.gain.load(Ordering::Relaxed)));
		let gain = if muted {
			0.0
		} else {
			10f32.powf(db as f32 / 20.0)
		};
		unsafe {
			let input = &*data.inputs;
			let output = &*data.outputs;
			for c in 0..2 {
				let src = std::slice::from_raw_parts(*input.__field0.channelBuffers32.add(c), n);
				let dst =
					std::slice::from_raw_parts_mut(*output.__field0.channelBuffers32.add(c), n);
				for (o, i) in dst.iter_mut().zip(src) {
					*o = gain * i;
				}
			}
		}
		kResultOk
	}

	unsafe fn getTailSamples(&self) -> u32 {
		0
	}
}

struct GainController {
	gain: Cell<f64>,
	mute: Cell<f64>,
	handler: Rc<RefCell<Option<ComPtr<IComponentHandler>>>>,
}

/// State shared by the test editor view and its timer.
#[derive(Default)]
struct ViewState {
	handler: Rc<RefCell<Option<ComPtr<IComponentHandler>>>>,
	frame: RefCell<Option<ComPtr<IPlugFrame>>>,
	/// The view itself (no reference held: it owns this state).
	view: Cell<*mut IPlugView>,
	ticks: Cell<u32>,
	size: Cell<(i32, i32)>,
}

struct ViewTimer(Rc<ViewState>);

impl Class for ViewTimer {
	type Interfaces = (ITimerHandler,);
}

impl ITimerHandlerTrait for ViewTimer {
	unsafe fn onTimer(&self) {
		let s = &self.0;
		s.ticks.set(s.ticks.get() + 1);
		match s.ticks.get() {
			1 => {
				if let Some(h) = s.handler.borrow().as_ref() {
					unsafe {
						h.beginEdit(0);
						h.performEdit(0, db_to_normalized(-6.0));
						h.endEdit(0);
					}
				}
			}
			2 => {
				let frame = s.frame.borrow().clone();
				if let Some(frame) = frame {
					let mut rect = ViewRect {
						left: 0,
						top: 0,
						right: 320,
						bottom: 240,
					};
					unsafe { frame.resizeView(s.view.get(), &mut rect) };
				}
			}
			_ => {}
		}
	}
}

struct GainView {
	state: Rc<ViewState>,
	timer: ComWrapper<ViewTimer>,
}

impl Class for GainView {
	type Interfaces = (IPlugView,);
}

impl GainView {
	fn timer_ptr(&self) -> *mut ITimerHandler {
		self.timer
			.as_com_ref::<ITimerHandler>()
			.map_or(std::ptr::null_mut(), |r| r.as_ptr())
	}

	fn run_loop(&self) -> Option<ComPtr<IRunLoop>> {
		self.state.frame.borrow().as_ref()?.cast::<IRunLoop>()
	}
}

impl IPlugViewTrait for GainView {
	unsafe fn isPlatformTypeSupported(&self, r#type: FIDString) -> tresult {
		let wanted = unsafe { std::ffi::CStr::from_ptr(kPlatformTypeX11EmbedWindowID) };
		if !r#type.is_null() && unsafe { std::ffi::CStr::from_ptr(r#type) } == wanted {
			kResultTrue
		} else {
			kResultFalse
		}
	}

	unsafe fn attached(&self, parent: *mut c_void, _type: FIDString) -> tresult {
		if parent.is_null() {
			return kInvalidArgument;
		}
		if let Some(run_loop) = self.run_loop() {
			unsafe { run_loop.registerTimer(self.timer_ptr(), 10) };
		}
		kResultOk
	}

	unsafe fn removed(&self) -> tresult {
		if let Some(run_loop) = self.run_loop() {
			unsafe { run_loop.unregisterTimer(self.timer_ptr()) };
		}
		kResultOk
	}

	unsafe fn onWheel(&self, _distance: f32) -> tresult {
		kResultFalse
	}

	unsafe fn onKeyDown(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
		kResultFalse
	}

	unsafe fn onKeyUp(&self, _key: char16, _code: int16, _modifiers: int16) -> tresult {
		kResultFalse
	}

	unsafe fn getSize(&self, size: *mut ViewRect) -> tresult {
		if size.is_null() {
			return kInvalidArgument;
		}
		let (w, h) = self.state.size.get();
		unsafe {
			*size = ViewRect {
				left: 0,
				top: 0,
				right: w,
				bottom: h,
			}
		};
		kResultOk
	}

	unsafe fn onSize(&self, size: *mut ViewRect) -> tresult {
		if size.is_null() {
			return kInvalidArgument;
		}
		let r = unsafe { *size };
		self.state.size.set((r.right - r.left, r.bottom - r.top));
		kResultOk
	}

	unsafe fn onFocus(&self, _state: TBool) -> tresult {
		kResultOk
	}

	unsafe fn setFrame(&self, frame: *mut IPlugFrame) -> tresult {
		*self.state.frame.borrow_mut() = unsafe { ComRef::from_raw(frame) }.map(|f| f.to_com_ptr());
		kResultOk
	}

	unsafe fn canResize(&self) -> tresult {
		kResultTrue
	}

	unsafe fn checkSizeConstraint(&self, _rect: *mut ViewRect) -> tresult {
		kResultOk
	}
}

impl Class for GainController {
	type Interfaces = (IEditController, IConnectionPoint, IUnitInfo);
}

impl IUnitInfoTrait for GainController {
	unsafe fn getUnitCount(&self) -> i32 {
		1
	}

	unsafe fn getUnitInfo(&self, index: i32, info: *mut UnitInfo) -> tresult {
		if index != 0 {
			return kInvalidArgument;
		}
		let info = unsafe { &mut *info };
		info.id = kRootUnitId;
		info.parentUnitId = kNoParentUnitId;
		copy_wstring("Root", &mut info.name);
		info.programListId = 1;
		kResultOk
	}

	unsafe fn getProgramListCount(&self) -> i32 {
		1
	}

	unsafe fn getProgramListInfo(&self, index: i32, info: *mut ProgramListInfo) -> tresult {
		if index != 0 {
			return kInvalidArgument;
		}
		let info = unsafe { &mut *info };
		info.id = 1;
		copy_wstring("Factory", &mut info.name);
		info.programCount = PROGRAMS.len() as i32;
		kResultOk
	}

	unsafe fn getProgramName(&self, list: i32, index: i32, name: *mut String128) -> tresult {
		match PROGRAMS.get(index as usize) {
			Some((n, _)) if list == 1 => {
				copy_wstring(n, unsafe { &mut *name });
				kResultOk
			}
			_ => kInvalidArgument,
		}
	}

	unsafe fn getProgramInfo(
		&self,
		_l: i32,
		_i: i32,
		_a: vst3::Steinberg::Vst::CString,
		_v: *mut String128,
	) -> tresult {
		kNotImplemented
	}

	unsafe fn hasProgramPitchNames(&self, _l: i32, _i: i32) -> tresult {
		kResultFalse
	}

	unsafe fn getProgramPitchName(&self, _l: i32, _i: i32, _p: i16, _n: *mut String128) -> tresult {
		kNotImplemented
	}

	unsafe fn getSelectedUnit(&self) -> i32 {
		kRootUnitId
	}

	unsafe fn selectUnit(&self, _unit: i32) -> tresult {
		kResultOk
	}

	unsafe fn getUnitByBus(
		&self,
		_t: MediaType,
		_d: BusDirection,
		_b: i32,
		_c: i32,
		unit: *mut i32,
	) -> tresult {
		unsafe { *unit = kRootUnitId };
		kResultOk
	}

	unsafe fn setUnitProgramData(&self, _l: i32, _i: i32, _d: *mut IBStream) -> tresult {
		kNotImplemented
	}
}

impl GainController {
	const CID: TUID = uid(0x43444E5A, 0x56334741, 0x494E4354, 0x524C0002);
}

impl IPluginBaseTrait for GainController {
	unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
		kResultOk
	}

	unsafe fn terminate(&self) -> tresult {
		kResultOk
	}
}

impl IConnectionPointTrait for GainController {
	unsafe fn connect(&self, _other: *mut IConnectionPoint) -> tresult {
		kResultOk
	}

	unsafe fn disconnect(&self, _other: *mut IConnectionPoint) -> tresult {
		kResultOk
	}

	unsafe fn notify(&self, _message: *mut IMessage) -> tresult {
		kResultOk
	}
}

impl IEditControllerTrait for GainController {
	unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
		match unsafe { read_f64s::<2>(state) } {
			Some([gain, mute]) => {
				self.gain.set(gain);
				self.mute.set(mute);
				kResultOk
			}
			None => kResultFalse,
		}
	}

	unsafe fn setState(&self, state: *mut IBStream) -> tresult {
		let Some(stream) = (unsafe { ComRef::from_raw(state) }) else {
			return kInvalidArgument;
		};
		let mut bytes = [0u8; 4];
		let mut read = 0;
		unsafe { stream.read(bytes.as_mut_ptr().cast(), 4, &mut read) };
		if &bytes == b"CTRL" {
			kResultOk
		} else {
			kResultFalse
		}
	}

	unsafe fn getState(&self, state: *mut IBStream) -> tresult {
		unsafe { write_bytes(state, b"CTRL") }
	}

	unsafe fn getParameterCount(&self) -> i32 {
		4
	}

	unsafe fn getParameterInfo(&self, index: i32, info: *mut ParameterInfo) -> tresult {
		let info = unsafe { &mut *info };
		use ParameterInfo_::ParameterFlags_ as F;
		let (title, units, steps, default, flags) = match index {
			0 => ("Gain", "dB", 0, db_to_normalized(0.0), F::kCanAutomate),
			1 => ("Mute", "", 1, 0.0, F::kCanAutomate),
			2 => ("Meter", "dB", 0, 0.0, F::kIsReadOnly),
			3 => (
				"Program",
				"",
				PROGRAMS.len() as i32 - 1,
				0.0,
				F::kIsProgramChange | F::kIsHidden | F::kIsList,
			),
			_ => return kInvalidArgument,
		};
		info.id = index as u32;
		copy_wstring(title, &mut info.title);
		copy_wstring(title, &mut info.shortTitle);
		copy_wstring(units, &mut info.units);
		info.stepCount = steps;
		info.defaultNormalizedValue = default;
		info.unitId = 0;
		info.flags = flags;
		kResultOk
	}

	unsafe fn getParamStringByValue(&self, _id: u32, _v: f64, _s: *mut String128) -> tresult {
		kNotImplemented
	}

	unsafe fn getParamValueByString(&self, _id: u32, _s: *mut TChar, _v: *mut f64) -> tresult {
		kNotImplemented
	}

	unsafe fn normalizedParamToPlain(&self, id: u32, value: f64) -> f64 {
		match id {
			0 | 2 => normalized_to_db(value),
			_ => value.clamp(0.0, 1.0).round(),
		}
	}

	unsafe fn plainParamToNormalized(&self, id: u32, plain: f64) -> f64 {
		match id {
			0 | 2 => db_to_normalized(plain),
			_ => plain.clamp(0.0, 1.0).round(),
		}
	}

	unsafe fn getParamNormalized(&self, id: u32) -> f64 {
		match id {
			0 => self.gain.get(),
			1 => self.mute.get(),
			_ => 0.0,
		}
	}

	unsafe fn setParamNormalized(&self, id: u32, value: f64) -> tresult {
		match id {
			0 => self.gain.set(value),
			1 => self.mute.set(value),
			3 => self
				.gain
				.set(db_to_normalized(PROGRAMS[program_at(value)].1)),
			_ => return kInvalidArgument,
		}
		kResultOk
	}

	unsafe fn setComponentHandler(&self, handler: *mut IComponentHandler) -> tresult {
		*self.handler.borrow_mut() = unsafe { ComRef::from_raw(handler) }.map(|h| h.to_com_ptr());
		kResultOk
	}

	unsafe fn createView(&self, name: *const c_char) -> *mut IPlugView {
		if name.is_null() || unsafe { std::ffi::CStr::from_ptr(name) } != c"editor" {
			return std::ptr::null_mut();
		}
		let state = Rc::new(ViewState {
			handler: self.handler.clone(),
			size: Cell::new((300, 200)),
			..ViewState::default()
		});
		let view = ComWrapper::new(GainView {
			timer: ComWrapper::new(ViewTimer(state.clone())),
			state: state.clone(),
		});
		let Some(ptr) = view.to_com_ptr::<IPlugView>() else {
			return std::ptr::null_mut();
		};
		state.view.set(ptr.as_ptr());
		ptr.into_raw()
	}
}

struct Factory;

impl Class for Factory {
	type Interfaces = (IPluginFactory2,);
}

impl IPluginFactoryTrait for Factory {
	unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
		let info = unsafe { &mut *info };
		copy_cstring("Cadenza", &mut info.vendor);
		copy_cstring("https://github.com/Pep3M/cadenza", &mut info.url);
		copy_cstring("", &mut info.email);
		info.flags = PFactoryInfo_::FactoryFlags_::kUnicode as int32;
		kResultOk
	}

	unsafe fn countClasses(&self) -> i32 {
		2
	}

	unsafe fn getClassInfo(&self, index: i32, info: *mut PClassInfo) -> tresult {
		let mut info2: PClassInfo2 = unsafe { std::mem::zeroed() };
		let r = unsafe { self.getClassInfo2(index, &mut info2) };
		if r == kResultOk {
			let info = unsafe { &mut *info };
			info.cid = info2.cid;
			info.cardinality = info2.cardinality;
			info.category = info2.category;
			info.name = info2.name;
		}
		r
	}

	unsafe fn createInstance(
		&self,
		cid: FIDString,
		iid: FIDString,
		obj: *mut *mut c_void,
	) -> tresult {
		let instance = match unsafe { *(cid as *const TUID) } {
			GainProcessor::CID => ComWrapper::new(GainProcessor {
				gain: AtomicU64::new(db_to_normalized(0.0).to_bits()),
				mute: AtomicU64::new(0f64.to_bits()),
				connected: AtomicBool::new(false),
			})
			.to_com_ptr::<FUnknown>(),
			GainController::CID => ComWrapper::new(GainController {
				gain: Cell::new(db_to_normalized(0.0)),
				mute: Cell::new(0.0),
				handler: Rc::default(),
			})
			.to_com_ptr::<FUnknown>(),
			_ => None,
		};
		match instance {
			Some(instance) => {
				let ptr = instance.as_ptr();
				unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid as *const TUID, obj) }
			}
			None => kInvalidArgument,
		}
	}
}

impl IPluginFactory2Trait for Factory {
	unsafe fn getClassInfo2(&self, index: i32, info: *mut PClassInfo2) -> tresult {
		let info = unsafe { &mut *info };
		let (cid, category) = match index {
			0 => (GainProcessor::CID, "Audio Module Class"),
			1 => (GainController::CID, "Component Controller Class"),
			_ => return kInvalidArgument,
		};
		info.cid = cid;
		info.cardinality = PClassInfo_::ClassCardinality_::kManyInstances as int32;
		copy_cstring(category, &mut info.category);
		copy_cstring("Cadenza Test VST3 Gain", &mut info.name);
		info.classFlags = 0;
		copy_cstring("Fx|Dynamics", &mut info.subCategories);
		copy_cstring("Cadenza", &mut info.vendor);
		copy_cstring("1.0.0", &mut info.version);
		copy_cstring("VST 3.7", &mut info.sdkVersion);
		kResultOk
	}
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn InitDll() -> bool {
	true
}

#[cfg(target_os = "windows")]
#[unsafe(no_mangle)]
extern "system" fn ExitDll() -> bool {
	true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleEntry(_bundle_ref: *mut c_void) -> bool {
	true
}

#[cfg(target_os = "macos")]
#[unsafe(no_mangle)]
extern "system" fn bundleExit() -> bool {
	true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleEntry(_library_handle: *mut c_void) -> bool {
	true
}

#[cfg(target_os = "linux")]
#[unsafe(no_mangle)]
extern "system" fn ModuleExit() -> bool {
	true
}

#[unsafe(no_mangle)]
extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
	ComWrapper::new(Factory)
		.to_com_ptr::<IPluginFactory>()
		.map_or(std::ptr::null_mut(), |p| p.into_raw())
}
