//! Loading a VST3 module (bundle or legacy file): platform entry points,
//! the plugin factory and its class list.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use vst3::Steinberg::kResultOk;
use vst3::Steinberg::{
	FIDString, FUnknown, IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactory3,
	IPluginFactory3Trait, IPluginFactoryTrait, PClassInfo, PClassInfo2, PClassInfoW, PFactoryInfo,
	TUID,
};
use vst3::{ComPtr, ComWrapper, Interface};

use super::com::{HostApplication, from_c, from_utf16};
use crate::discovery::vst3_binary;
use crate::error::PluginError;

/// VST3 class category of audio processors (the only classes Cadenza hosts).
pub(crate) const AUDIO_MODULE_CLASS: &str = "Audio Module Class";

/// One class exported by a module's factory.
#[derive(Debug, Clone)]
pub(crate) struct ClassInfo {
	pub cid: TUID,
	pub category: String,
	pub name: String,
	pub vendor: String,
	pub version: String,
	/// `|`-separated sub-categories, e.g. "Fx|Reverb" or "Instrument|Synth".
	pub sub_categories: String,
}

type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
type ExitFn = unsafe extern "system" fn() -> bool;

/// A loaded module. Dropping it releases the factory, calls the platform exit
/// function and unloads the library, in that order; callers keep it alive
/// (refcounted per path) for as long as any instance from it exists.
pub(crate) struct Module {
	path: PathBuf,
	factory: Option<ComPtr<IPluginFactory>>,
	/// Host context given to `IPluginFactory3::setHostContext` and to every
	/// instance's `initialize`.
	pub host: ComWrapper<HostApplication>,
	pub factory_vendor: String,
	exit: Option<ExitFn>,
	#[cfg(target_os = "macos")]
	bundle_ref: *mut c_void,
	library: Option<libloading::Library>,
}

fn load_err(path: &Path, reason: impl Into<String>) -> PluginError {
	PluginError::Load {
		path: path.to_path_buf(),
		format: "VST3",
		reason: reason.into(),
	}
}

#[cfg(target_os = "macos")]
mod cf {
	use std::ffi::c_void;
	use std::path::Path;

	#[link(name = "CoreFoundation", kind = "framework")]
	unsafe extern "C" {
		fn CFURLCreateFromFileSystemRepresentation(
			allocator: *const c_void,
			buffer: *const u8,
			len: isize,
			is_directory: u8,
		) -> *const c_void;
		fn CFBundleCreate(allocator: *const c_void, url: *const c_void) -> *mut c_void;
		pub fn CFRelease(cf: *const c_void);
	}

	/// A `CFBundleRef` for the bundle directory (null on failure).
	pub fn bundle_ref(path: &Path) -> *mut c_void {
		use std::os::unix::ffi::OsStrExt;
		let bytes = path.as_os_str().as_bytes();
		// SAFETY: plain CoreFoundation calls with a valid byte buffer.
		unsafe {
			let url = CFURLCreateFromFileSystemRepresentation(
				std::ptr::null(),
				bytes.as_ptr(),
				bytes.len() as isize,
				1,
			);
			if url.is_null() {
				return std::ptr::null_mut();
			}
			let bundle = CFBundleCreate(std::ptr::null(), url);
			CFRelease(url);
			bundle
		}
	}
}

impl Module {
	/// Loads the module at `path` (a `.vst3` bundle directory or file) and
	/// runs its entry function. Runs foreign code: only call it from the
	/// scanner subprocess or for files the scanner already accepted.
	pub fn load(path: &Path) -> Result<Module, PluginError> {
		let binary = vst3_binary(path).ok_or_else(|| {
			load_err(
				path,
				format!(
					"no binary for this platform inside the bundle (expected Contents/{}/)",
					crate::discovery::vst3_arch_dir()
				),
			)
		})?;
		let (library, exit, bundle) = Self::open(path, &binary)?;
		#[cfg(not(target_os = "macos"))]
		let _ = bundle;

		let mut module = Module {
			path: path.to_path_buf(),
			factory: None,
			host: ComWrapper::new(HostApplication),
			factory_vendor: String::new(),
			exit,
			#[cfg(target_os = "macos")]
			bundle_ref: bundle,
			library: Some(library),
		};
		// From here on, `Drop` runs the exit function if anything fails.
		let get_factory: GetFactory = {
			let lib = module.library.as_ref().expect("library just stored");
			// SAFETY: symbol type per the VST3 module ABI.
			*unsafe { lib.get::<GetFactory>(b"GetPluginFactory\0") }
				.map_err(|e| load_err(path, format!("missing GetPluginFactory ({e})")))?
		};
		// SAFETY: the module is initialised; the returned pointer carries a reference.
		let factory = unsafe { ComPtr::from_raw(get_factory()) }
			.ok_or_else(|| load_err(path, "GetPluginFactory returned null"))?;
		if let Some(f3) = factory.cast::<IPluginFactory3>() {
			let ctx = module
				.host
				.as_com_ref::<FUnknown>()
				.map_or(std::ptr::null_mut(), |r| r.as_ptr());
			// SAFETY: valid host context; the module keeps it alive.
			unsafe { f3.setHostContext(ctx) };
		}
		// SAFETY: PFactoryInfo is plain C data; zeroed is valid.
		let mut info: PFactoryInfo = unsafe { std::mem::zeroed() };
		// SAFETY: valid out-parameter.
		if unsafe { factory.getFactoryInfo(&mut info) } == kResultOk {
			module.factory_vendor = from_c(&info.vendor);
		}
		module.factory = Some(factory);
		Ok(module)
	}

	#[cfg(all(unix, not(target_os = "macos")))]
	fn open(
		path: &Path,
		binary: &Path,
	) -> Result<(libloading::Library, Option<ExitFn>, *mut c_void), PluginError> {
		use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
		// SAFETY: loading foreign code is inherent to in-process hosting.
		let lib = unsafe { Library::open(Some(binary), RTLD_NOW | RTLD_LOCAL) }
			.map_err(|e| load_err(path, e.to_string()))?;
		let handle = lib.into_raw();
		// SAFETY: `handle` was just produced by `into_raw`.
		let lib = unsafe { Library::from_raw(handle) };
		type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
		// SAFETY: symbol types per the VST3 Linux module ABI.
		unsafe {
			if let Ok(entry) = lib.get::<Entry>(b"ModuleEntry\0")
				&& !entry(handle)
			{
				return Err(load_err(path, "ModuleEntry returned false"));
			}
			let exit = lib.get::<ExitFn>(b"ModuleExit\0").ok().map(|s| *s);
			Ok((lib.into(), exit, std::ptr::null_mut()))
		}
	}

	#[cfg(windows)]
	fn open(
		path: &Path,
		binary: &Path,
	) -> Result<(libloading::Library, Option<ExitFn>, *mut c_void), PluginError> {
		// SAFETY: loading foreign code is inherent to in-process hosting.
		let lib = unsafe { libloading::Library::new(binary) }
			.map_err(|e| load_err(path, e.to_string()))?;
		// SAFETY: symbol types per the VST3 Windows module ABI.
		unsafe {
			if let Ok(init) = lib.get::<unsafe extern "system" fn() -> bool>(b"InitDll\0")
				&& !init()
			{
				return Err(load_err(path, "InitDll returned false"));
			}
			let exit = lib.get::<ExitFn>(b"ExitDll\0").ok().map(|s| *s);
			Ok((lib, exit, std::ptr::null_mut()))
		}
	}

	#[cfg(target_os = "macos")]
	fn open(
		path: &Path,
		binary: &Path,
	) -> Result<(libloading::Library, Option<ExitFn>, *mut c_void), PluginError> {
		// SAFETY: loading foreign code is inherent to in-process hosting.
		let lib = unsafe { libloading::Library::new(binary) }
			.map_err(|e| load_err(path, e.to_string()))?;
		let bundle = cf::bundle_ref(path);
		type Entry = unsafe extern "system" fn(*mut c_void) -> bool;
		// SAFETY: symbol types per the VST3 macOS module ABI.
		unsafe {
			let entry = lib
				.get::<Entry>(b"bundleEntry\0")
				.or_else(|_| lib.get::<Entry>(b"BundleEntry\0"));
			if let Ok(entry) = entry
				&& !entry(bundle)
			{
				if !bundle.is_null() {
					cf::CFRelease(bundle);
				}
				return Err(load_err(path, "bundleEntry returned false"));
			}
			let exit = lib
				.get::<ExitFn>(b"bundleExit\0")
				.or_else(|_| lib.get::<ExitFn>(b"BundleExit\0"))
				.ok()
				.map(|s| *s);
			Ok((lib, exit, bundle))
		}
	}

	pub fn path(&self) -> &Path {
		&self.path
	}

	fn factory(&self) -> &ComPtr<IPluginFactory> {
		self.factory.as_ref().expect("factory exists while loaded")
	}

	/// Every class the factory exports, with the richest info available
	/// (`IPluginFactory3` Unicode info, then `IPluginFactory2`, then v1).
	pub fn classes(&self) -> Vec<ClassInfo> {
		let factory = self.factory();
		let f2 = factory.cast::<IPluginFactory2>();
		let f3 = factory.cast::<IPluginFactory3>();
		// SAFETY: plain factory queries with valid out-parameters.
		let count = unsafe { factory.countClasses() }.clamp(0, 4096);
		let mut out = Vec::new();
		for i in 0..count {
			unsafe {
				if let Some(f3) = &f3 {
					let mut info: PClassInfoW = std::mem::zeroed();
					if f3.getClassInfoUnicode(i, &mut info) == kResultOk {
						out.push(ClassInfo {
							cid: info.cid,
							category: from_c(&info.category),
							name: from_utf16(&info.name),
							vendor: from_utf16(&info.vendor),
							version: from_utf16(&info.version),
							sub_categories: from_c(&info.subCategories),
						});
						continue;
					}
				}
				if let Some(f2) = &f2 {
					let mut info: PClassInfo2 = std::mem::zeroed();
					if f2.getClassInfo2(i, &mut info) == kResultOk {
						out.push(ClassInfo {
							cid: info.cid,
							category: from_c(&info.category),
							name: from_c(&info.name),
							vendor: from_c(&info.vendor),
							version: from_c(&info.version),
							sub_categories: from_c(&info.subCategories),
						});
						continue;
					}
				}
				let mut info: PClassInfo = std::mem::zeroed();
				if factory.getClassInfo(i, &mut info) == kResultOk {
					out.push(ClassInfo {
						cid: info.cid,
						category: from_c(&info.category),
						name: from_c(&info.name),
						vendor: String::new(),
						version: String::new(),
						sub_categories: String::new(),
					});
				}
			}
		}
		out
	}

	/// Creates an instance of class `cid` and returns interface `I` of it.
	pub fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
		let mut obj: *mut c_void = std::ptr::null_mut();
		// SAFETY: valid class/interface ids and out-parameter; on success the
		// object comes with one reference that `ComPtr` takes over.
		unsafe {
			let r = self.factory().createInstance(
				cid.as_ptr() as FIDString,
				I::IID.as_ptr() as FIDString,
				&mut obj,
			);
			if r != kResultOk {
				return None;
			}
			ComPtr::from_raw(obj as *mut I)
		}
	}
}

impl Drop for Module {
	fn drop(&mut self) {
		drop(self.factory.take());
		if let Some(exit) = self.exit {
			// SAFETY: module entry succeeded, so its exit function must be called
			// exactly once before unloading.
			unsafe { exit() };
		}
		#[cfg(target_os = "macos")]
		if !self.bundle_ref.is_null() {
			// SAFETY: created by CFBundleCreate in `open`.
			unsafe { cf::CFRelease(self.bundle_ref) };
		}
		drop(self.library.take());
	}
}
