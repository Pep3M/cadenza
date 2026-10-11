//! Host-side COM objects handed to VST3 plugins (host context, component
//! handler, messages, attribute lists, memory streams) and small FFI helpers.
//!
//! Every method a plugin can call is wrapped in [`guard`], so a Rust panic
//! becomes an error code instead of unwinding into C++ (undefined behaviour).

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use vst3::Steinberg::Vst::{
	IAttributeList, IAttributeListTrait, IAudioProcessor, IComponent, IComponentHandler,
	IComponentHandler2, IComponentHandler2Trait, IComponentHandlerTrait, IConnectionPoint,
	IEditController, IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait,
	IPlugInterfaceSupport, IPlugInterfaceSupportTrait, ParamID, ParamValue, RestartFlags_,
	String128, TChar,
};
use vst3::Steinberg::{
	FIDString, FUnknown, IBStream, IBStream_::IStreamSeekMode_, IBStreamTrait, TUID, int32, int64,
	tresult, uint32,
};
use vst3::Steinberg::{kInvalidArgument, kNoInterface, kResultFalse, kResultOk};
use vst3::{Class, ComPtr, ComWrapper, Interface};

/// Runs a host callback body, turning a panic into `fallback` so it never
/// unwinds across the FFI boundary.
pub(crate) fn guard<R>(fallback: R, f: impl FnOnce() -> R) -> R {
	catch_unwind(AssertUnwindSafe(f)).unwrap_or(fallback)
}

/// Moves a value that holds COM pointers between the caller and the plugin
/// main thread.
///
/// SAFETY contract (upheld by every user in this module tree): the wrapped
/// pointers are only *used* on one thread at a time — main-thread interfaces
/// on `cadenza-plugin-main` (or the scanner's thread), `IAudioProcessor` on
/// whichever thread currently owns the `Processor` exclusively. Reference
/// counting itself is thread-safe in every VST3 implementation.
pub(crate) struct SendPtr<T>(pub T);

unsafe impl<T> Send for SendPtr<T> {}

/// Converts a NUL-terminated UTF-16 buffer to a `String`.
pub(crate) fn from_utf16(buf: &[TChar]) -> String {
	let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
	String::from_utf16_lossy(&buf[..len]).trim().to_string()
}

/// Converts a NUL-terminated 8-bit buffer to a `String`.
pub(crate) fn from_c(buf: &[c_char]) -> String {
	let bytes: Vec<u8> = buf
		.iter()
		.take_while(|&&c| c != 0)
		.map(|&c| c as u8)
		.collect();
	String::from_utf8_lossy(&bytes).trim().to_string()
}

/// Writes `s` into a fixed UTF-16 buffer, truncating and NUL-terminating.
pub(crate) fn write_utf16(dst: &mut [TChar], s: &str) {
	let Some(max) = dst.len().checked_sub(1) else {
		return;
	};
	let mut n = 0;
	for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(max)) {
		*d = c;
		n += 1;
	}
	dst[n] = 0;
}

/// A class id as 32 uppercase hex digits, in memory byte order (the form
/// `moduleinfo.json` uses on non-Windows platforms).
pub(crate) fn tuid_to_hex(tuid: &TUID) -> String {
	tuid.iter().map(|&b| format!("{:02X}", b as u8)).collect()
}

/// Parses [`tuid_to_hex`] output.
pub(crate) fn tuid_from_hex(s: &str) -> Option<TUID> {
	let s = s.trim();
	if s.len() != 32 || !s.is_ascii() {
		return None;
	}
	let mut out = [0 as c_char; 16];
	for (i, o) in out.iter_mut().enumerate() {
		*o = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()? as c_char;
	}
	Some(out)
}

fn same_iid<I: Interface>(tuid: &TUID) -> bool {
	tuid.iter().zip(I::IID.iter()).all(|(&a, &b)| a as u8 == b)
}

/// Hands `object` out through an `(iid, **obj)` pair, as `queryInterface` does.
unsafe fn query_out(object: &ComPtr<FUnknown>, iid: *const TUID, obj: *mut *mut c_void) -> tresult {
	let ptr = object.as_ptr();
	// SAFETY: `ptr` is a live object we own a reference to; the plugin
	// supplied `iid`/`obj` per the COM contract.
	unsafe { ((*(*ptr).vtbl).queryInterface)(ptr, iid, obj) }
}

// ------------------------------------------------------------ host context

/// `IHostApplication` + `IPlugInterfaceSupport`: the context passed to
/// `initialize` and `IPluginFactory3::setHostContext`.
pub(crate) struct HostApplication;

impl Class for HostApplication {
	type Interfaces = (IHostApplication, IPlugInterfaceSupport);
}

impl IHostApplicationTrait for HostApplication {
	unsafe fn getName(&self, name: *mut String128) -> tresult {
		guard(kResultFalse, || {
			if name.is_null() {
				return kInvalidArgument;
			}
			// SAFETY: non-null pointer to a String128 owned by the caller.
			write_utf16(unsafe { &mut *name }, "Cadenza");
			kResultOk
		})
	}

	unsafe fn createInstance(
		&self,
		cid: *mut TUID,
		iid: *mut TUID,
		obj: *mut *mut c_void,
	) -> tresult {
		guard(kResultFalse, || {
			if cid.is_null() || iid.is_null() || obj.is_null() {
				return kInvalidArgument;
			}
			// SAFETY: checked non-null; points to a 16-byte TUID.
			let cid = unsafe { &*cid };
			let object = if same_iid::<IMessage>(cid) {
				ComWrapper::new(HostMessage::default()).to_com_ptr::<FUnknown>()
			} else if same_iid::<IAttributeList>(cid) {
				ComWrapper::new(HostAttributeList::default()).to_com_ptr::<FUnknown>()
			} else {
				None
			};
			match object {
				// SAFETY: forwarding the plugin's out-parameters.
				Some(object) => unsafe { query_out(&object, iid, obj) },
				None => {
					// SAFETY: checked non-null.
					unsafe { *obj = std::ptr::null_mut() };
					kNoInterface
				}
			}
		})
	}
}

impl IPlugInterfaceSupportTrait for HostApplication {
	unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
		guard(kResultFalse, || {
			if iid.is_null() {
				return kInvalidArgument;
			}
			// SAFETY: checked non-null.
			let iid = unsafe { &*iid };
			let supported = same_iid::<IComponent>(iid)
				|| same_iid::<IAudioProcessor>(iid)
				|| same_iid::<IEditController>(iid)
				|| same_iid::<IConnectionPoint>(iid);
			if supported { kResultOk } else { kResultFalse }
		})
	}
}

// ------------------------------------------------------- component handler

/// Requests a plugin made through `IComponentHandler`, consumed by the host
/// (on the next `prepare`, like the CLAP host flags).
#[derive(Debug, Default)]
pub(crate) struct Vst3Flags {
	/// Re-activate before the next process (`kReloadComponent`, `kIoChanged`,
	/// `kLatencyChanged`).
	pub restart: AtomicBool,
	pub latency_changed: AtomicBool,
	pub params_changed: AtomicBool,
	pub state_dirty: AtomicBool,
}

impl Vst3Flags {
	pub fn take(flag: &AtomicBool) -> bool {
		flag.swap(false, Ordering::AcqRel)
	}
}

/// `IComponentHandler(2)`: edits from the plugin's own UI are ignored for now
/// (there is no editor yet); restart requests only flip atomic flags, so the
/// plugin may call them from any thread.
pub(crate) struct ComponentHandler {
	pub flags: Arc<Vst3Flags>,
}

impl Class for ComponentHandler {
	type Interfaces = (IComponentHandler, IComponentHandler2);
}

impl IComponentHandlerTrait for ComponentHandler {
	unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
		kResultOk
	}

	unsafe fn performEdit(&self, _id: ParamID, _value: ParamValue) -> tresult {
		kResultOk
	}

	unsafe fn endEdit(&self, _id: ParamID) -> tresult {
		kResultOk
	}

	unsafe fn restartComponent(&self, flags: int32) -> tresult {
		guard(kResultFalse, || {
			let flags = flags as u32;
			let set = |f: &AtomicBool| f.store(true, Ordering::Release);
			if flags & RestartFlags_::kLatencyChanged as u32 != 0 {
				set(&self.flags.latency_changed);
				set(&self.flags.restart);
			}
			if flags & (RestartFlags_::kReloadComponent | RestartFlags_::kIoChanged) as u32 != 0 {
				set(&self.flags.restart);
			}
			if flags
				& (RestartFlags_::kParamValuesChanged | RestartFlags_::kParamTitlesChanged) as u32
				!= 0
			{
				set(&self.flags.params_changed);
			}
			kResultOk
		})
	}
}

impl IComponentHandler2Trait for ComponentHandler {
	unsafe fn setDirty(&self, state: u8) -> tresult {
		if state != 0 {
			self.flags.state_dirty.store(true, Ordering::Release);
		}
		kResultOk
	}

	unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
		kResultFalse
	}

	unsafe fn startGroupEdit(&self) -> tresult {
		kResultOk
	}

	unsafe fn finishGroupEdit(&self) -> tresult {
		kResultOk
	}
}

// ----------------------------------------------------- messages/attributes

/// `IMessage` created through `IHostApplication::createInstance`; plugins with
/// separate component/controller classes use it to talk over
/// `IConnectionPoint`.
pub(crate) struct HostMessage {
	id: Mutex<Option<CString>>,
	attributes: ComWrapper<HostAttributeList>,
}

impl Default for HostMessage {
	fn default() -> Self {
		Self {
			id: Mutex::new(None),
			attributes: ComWrapper::new(HostAttributeList::default()),
		}
	}
}

impl Class for HostMessage {
	type Interfaces = (IMessage,);
}

impl IMessageTrait for HostMessage {
	unsafe fn getMessageID(&self) -> FIDString {
		guard(std::ptr::null(), || {
			let id = self.id.lock().unwrap_or_else(|e| e.into_inner());
			// The pointer stays valid until the next `setMessageID`, as in the SDK.
			id.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())
		})
	}

	unsafe fn setMessageID(&self, id: FIDString) {
		guard((), || {
			let value = (!id.is_null())
				// SAFETY: plugin passes a NUL-terminated string.
				.then(|| unsafe { CStr::from_ptr(id) }.to_owned());
			*self.id.lock().unwrap_or_else(|e| e.into_inner()) = value;
		})
	}

	unsafe fn getAttributes(&self) -> *mut IAttributeList {
		guard(std::ptr::null_mut(), || {
			// Borrowed pointer (no add-ref), matching the SDK's HostMessage.
			self.attributes
				.as_com_ref::<IAttributeList>()
				.map_or(std::ptr::null_mut(), |r| r.as_ptr())
		})
	}
}

enum Attribute {
	Int(i64),
	Float(f64),
	/// UTF-16 including the terminating NUL.
	String(Vec<TChar>),
	Binary(Vec<u8>),
}

/// `IAttributeList` backing [`HostMessage`].
#[derive(Default)]
pub(crate) struct HostAttributeList {
	values: Mutex<HashMap<CString, Attribute>>,
}

impl Class for HostAttributeList {
	type Interfaces = (IAttributeList,);
}

impl HostAttributeList {
	fn set(&self, id: *const c_char, value: Attribute) -> tresult {
		if id.is_null() {
			return kInvalidArgument;
		}
		// SAFETY: plugin passes a NUL-terminated attribute id.
		let key = unsafe { CStr::from_ptr(id) }.to_owned();
		self.values
			.lock()
			.unwrap_or_else(|e| e.into_inner())
			.insert(key, value);
		kResultOk
	}

	fn with<R>(&self, id: *const c_char, f: impl FnOnce(&Attribute) -> Option<R>) -> Option<R> {
		if id.is_null() {
			return None;
		}
		// SAFETY: plugin passes a NUL-terminated attribute id.
		let key = unsafe { CStr::from_ptr(id) };
		let values = self.values.lock().unwrap_or_else(|e| e.into_inner());
		values.get(key).and_then(f)
	}
}

impl IAttributeListTrait for HostAttributeList {
	unsafe fn setInt(&self, id: *const c_char, value: int64) -> tresult {
		guard(kResultFalse, || self.set(id, Attribute::Int(value)))
	}

	unsafe fn getInt(&self, id: *const c_char, value: *mut int64) -> tresult {
		guard(kResultFalse, || {
			match self.with(id, |a| match a {
				Attribute::Int(v) => Some(*v),
				_ => None,
			}) {
				Some(v) if !value.is_null() => {
					// SAFETY: checked non-null out-parameter.
					unsafe { *value = v };
					kResultOk
				}
				_ => kResultFalse,
			}
		})
	}

	unsafe fn setFloat(&self, id: *const c_char, value: f64) -> tresult {
		guard(kResultFalse, || self.set(id, Attribute::Float(value)))
	}

	unsafe fn getFloat(&self, id: *const c_char, value: *mut f64) -> tresult {
		guard(kResultFalse, || {
			match self.with(id, |a| match a {
				Attribute::Float(v) => Some(*v),
				_ => None,
			}) {
				Some(v) if !value.is_null() => {
					// SAFETY: checked non-null out-parameter.
					unsafe { *value = v };
					kResultOk
				}
				_ => kResultFalse,
			}
		})
	}

	unsafe fn setString(&self, id: *const c_char, string: *const TChar) -> tresult {
		guard(kResultFalse, || {
			if string.is_null() {
				return kInvalidArgument;
			}
			let mut s = Vec::new();
			// SAFETY: plugin passes a NUL-terminated UTF-16 string.
			unsafe {
				let mut p = string;
				while *p != 0 {
					s.push(*p);
					p = p.add(1);
				}
			}
			s.push(0);
			self.set(id, Attribute::String(s))
		})
	}

	unsafe fn getString(
		&self,
		id: *const c_char,
		string: *mut TChar,
		size_in_bytes: uint32,
	) -> tresult {
		guard(kResultFalse, || {
			let capacity = size_in_bytes as usize / std::mem::size_of::<TChar>();
			if string.is_null() || capacity == 0 {
				return kInvalidArgument;
			}
			let copied = self.with(id, |a| match a {
				Attribute::String(s) => {
					// `s` ends with its NUL; truncate and re-terminate if needed.
					let n = s.len().min(capacity);
					// SAFETY: `string` has room for `capacity` TChars.
					let dst = unsafe { std::slice::from_raw_parts_mut(string, capacity) };
					dst[..n].copy_from_slice(&s[..n]);
					dst[n - 1] = 0;
					Some(())
				}
				_ => None,
			});
			if copied.is_some() {
				kResultOk
			} else {
				kResultFalse
			}
		})
	}

	unsafe fn setBinary(
		&self,
		id: *const c_char,
		data: *const c_void,
		size_in_bytes: uint32,
	) -> tresult {
		guard(kResultFalse, || {
			let bytes = if data.is_null() || size_in_bytes == 0 {
				Vec::new()
			} else {
				// SAFETY: plugin passes `size_in_bytes` readable bytes.
				unsafe { std::slice::from_raw_parts(data as *const u8, size_in_bytes as usize) }
					.to_vec()
			};
			self.set(id, Attribute::Binary(bytes))
		})
	}

	unsafe fn getBinary(
		&self,
		id: *const c_char,
		data: *mut *const c_void,
		size_in_bytes: *mut uint32,
	) -> tresult {
		guard(kResultFalse, || {
			if data.is_null() || size_in_bytes.is_null() {
				return kInvalidArgument;
			}
			// The heap buffer stays put until the attribute is replaced, like
			// the SDK's HostAttributeList.
			match self.with(id, |a| match a {
				Attribute::Binary(b) => Some((b.as_ptr() as *const c_void, b.len() as uint32)),
				_ => None,
			}) {
				Some((ptr, len)) => {
					// SAFETY: checked non-null out-parameters.
					unsafe {
						*data = ptr;
						*size_in_bytes = len;
					}
					kResultOk
				}
				None => kResultFalse,
			}
		})
	}
}

// ------------------------------------------------------------ memory stream

/// `IBStream` over a growable byte buffer, used for state save/load.
#[derive(Default)]
pub(crate) struct MemoryStream {
	inner: Mutex<(Vec<u8>, usize)>,
}

impl Class for MemoryStream {
	type Interfaces = (IBStream,);
}

impl MemoryStream {
	pub fn with_data(data: Vec<u8>) -> ComWrapper<MemoryStream> {
		ComWrapper::new(MemoryStream {
			inner: Mutex::new((data, 0)),
		})
	}

	pub fn new() -> ComWrapper<MemoryStream> {
		Self::with_data(Vec::new())
	}

	pub fn take(&self) -> Vec<u8> {
		std::mem::take(&mut self.inner.lock().unwrap_or_else(|e| e.into_inner()).0)
	}

	pub fn rewind(&self) {
		self.inner.lock().unwrap_or_else(|e| e.into_inner()).1 = 0;
	}

	/// Borrowed `IBStream*` for passing to a plugin call.
	pub fn ptr(this: &ComWrapper<MemoryStream>) -> *mut IBStream {
		this.as_com_ref::<IBStream>()
			.map_or(std::ptr::null_mut(), |r| r.as_ptr())
	}
}

impl IBStreamTrait for MemoryStream {
	unsafe fn read(
		&self,
		buffer: *mut c_void,
		num_bytes: int32,
		num_bytes_read: *mut int32,
	) -> tresult {
		guard(kResultFalse, || {
			if num_bytes < 0 || (buffer.is_null() && num_bytes > 0) {
				return kInvalidArgument;
			}
			let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
			let (data, pos) = &mut *guard;
			let start = (*pos).min(data.len());
			let n = (num_bytes as usize).min(data.len() - start);
			if n > 0 {
				// SAFETY: the plugin provides `num_bytes` writable bytes.
				unsafe {
					std::ptr::copy_nonoverlapping(data[start..].as_ptr(), buffer as *mut u8, n)
				};
			}
			*pos = start + n;
			if !num_bytes_read.is_null() {
				// SAFETY: checked non-null out-parameter.
				unsafe { *num_bytes_read = n as int32 };
			}
			kResultOk
		})
	}

	unsafe fn write(
		&self,
		buffer: *mut c_void,
		num_bytes: int32,
		num_bytes_written: *mut int32,
	) -> tresult {
		guard(kResultFalse, || {
			if num_bytes < 0 || (buffer.is_null() && num_bytes > 0) {
				return kInvalidArgument;
			}
			let n = num_bytes as usize;
			let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
			let (data, pos) = &mut *guard;
			let end = *pos + n;
			if data.len() < end {
				data.resize(end, 0);
			}
			if n > 0 {
				// SAFETY: the plugin provides `num_bytes` readable bytes.
				let src = unsafe { std::slice::from_raw_parts(buffer as *const u8, n) };
				data[*pos..end].copy_from_slice(src);
			}
			*pos = end;
			if !num_bytes_written.is_null() {
				// SAFETY: checked non-null out-parameter.
				unsafe { *num_bytes_written = num_bytes };
			}
			kResultOk
		})
	}

	unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
		guard(kResultFalse, || {
			let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
			let (data, cur) = &mut *guard;
			let base = match mode as u32 {
				m if m == IStreamSeekMode_::kIBSeekSet as u32 => 0,
				m if m == IStreamSeekMode_::kIBSeekCur as u32 => *cur as i64,
				m if m == IStreamSeekMode_::kIBSeekEnd as u32 => data.len() as i64,
				_ => return kInvalidArgument,
			};
			let Some(target) = base.checked_add(pos).filter(|&t| t >= 0) else {
				return kInvalidArgument;
			};
			// Seeking past the end is allowed; a later write zero-fills the gap.
			*cur = target as usize;
			if !result.is_null() {
				// SAFETY: checked non-null out-parameter.
				unsafe { *result = target };
			}
			kResultOk
		})
	}

	unsafe fn tell(&self, pos: *mut int64) -> tresult {
		guard(kResultFalse, || {
			if pos.is_null() {
				return kInvalidArgument;
			}
			let cur = self.inner.lock().unwrap_or_else(|e| e.into_inner()).1;
			// SAFETY: checked non-null out-parameter.
			unsafe { *pos = cur as int64 };
			kResultOk
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use vst3::ComRef;

	#[test]
	fn memory_stream_reads_writes_and_seeks() {
		let stream = MemoryStream::new();
		let s: ComRef<IBStream> = stream.as_com_ref().unwrap();
		unsafe {
			let mut n = 0;
			let mut data = *b"hello";
			assert_eq!(s.write(data.as_mut_ptr().cast(), 5, &mut n), kResultOk);
			assert_eq!(n, 5);
			let mut pos = 0;
			s.tell(&mut pos);
			assert_eq!(pos, 5);
			s.seek(1, IStreamSeekMode_::kIBSeekSet as i32, &mut pos);
			let mut out = [0u8; 8];
			assert_eq!(s.read(out.as_mut_ptr().cast(), 8, &mut n), kResultOk);
			assert_eq!((n, &out[..4]), (4, &b"ello"[..]));
			// Reading at the end yields zero bytes, not an error.
			assert_eq!(s.read(out.as_mut_ptr().cast(), 8, &mut n), kResultOk);
			assert_eq!(n, 0);
			s.seek(-2, IStreamSeekMode_::kIBSeekEnd as i32, &mut pos);
			assert_eq!(pos, 3);
			let mut more = *b"LOW";
			s.write(more.as_mut_ptr().cast(), 3, &mut n);
			assert_eq!(
				s.seek(-100, IStreamSeekMode_::kIBSeekCur as i32, &mut pos),
				kInvalidArgument
			);
		}
		assert_eq!(stream.take(), b"helLOW");
	}

	#[test]
	fn attribute_list_round_trips() {
		let list = ComWrapper::new(HostAttributeList::default());
		let l: ComRef<IAttributeList> = list.as_com_ref().unwrap();
		let id = c"key";
		unsafe {
			assert_eq!(l.setInt(id.as_ptr(), 42), kResultOk);
			let mut v = 0;
			assert_eq!(l.getInt(id.as_ptr(), &mut v), kResultOk);
			assert_eq!(v, 42);
			let mut f = 0.0;
			assert_eq!(l.getFloat(id.as_ptr(), &mut f), kResultFalse);
			let text: Vec<u16> = "hi".encode_utf16().chain([0]).collect();
			l.setString(c"s".as_ptr(), text.as_ptr());
			let mut out = [7u16; 8];
			assert_eq!(l.getString(c"s".as_ptr(), out.as_mut_ptr(), 16), kResultOk);
			assert_eq!(&out[..3], &[b'h' as u16, b'i' as u16, 0]);
			let mut tiny = [7u16; 2];
			l.getString(c"s".as_ptr(), tiny.as_mut_ptr(), 4);
			assert_eq!(tiny, [b'h' as u16, 0]);
			let bytes = [1u8, 2, 3];
			l.setBinary(c"b".as_ptr(), bytes.as_ptr().cast(), 3);
			let mut ptr = std::ptr::null();
			let mut len = 0;
			assert_eq!(l.getBinary(c"b".as_ptr(), &mut ptr, &mut len), kResultOk);
			assert_eq!(
				std::slice::from_raw_parts(ptr as *const u8, len as usize),
				&bytes
			);
		}
	}

	#[test]
	fn host_creates_messages() {
		let host = ComWrapper::new(HostApplication);
		let h: ComRef<IHostApplication> = host.as_com_ref().unwrap();
		let mut cid: TUID = [0; 16];
		for (d, s) in cid.iter_mut().zip(IMessage::IID) {
			*d = s as c_char;
		}
		let mut iid = cid;
		let mut obj = std::ptr::null_mut();
		unsafe {
			assert_eq!(h.createInstance(&mut cid, &mut iid, &mut obj), kResultOk);
			let msg = ComPtr::<IMessage>::from_raw(obj as *mut IMessage).unwrap();
			msg.setMessageID(c"ping".as_ptr());
			assert_eq!(CStr::from_ptr(msg.getMessageID()), c"ping");
			let attrs = vst3::ComRef::from_raw(msg.getAttributes()).unwrap();
			assert_eq!(attrs.setFloat(c"x".as_ptr(), 1.5), kResultOk);
			let mut name = [0u16; 128];
			h.getName(&mut name);
			assert_eq!(from_utf16(&name), "Cadenza");
		}
	}

	#[test]
	fn tuid_hex_round_trip() {
		let hex = "64737020616270326D2020206162326D";
		let t = tuid_from_hex(hex).unwrap();
		assert_eq!(tuid_to_hex(&t), hex);
		assert_eq!(t[0] as u8, 0x64);
		assert!(tuid_from_hex("xyz").is_none());
	}
}
