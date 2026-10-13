//! A CLAP effect with an editor, for the plugin editor tests
//! (`dev.cadenza.test.gui`). Built as a separate `cdylib` example.
//!
//! Params (plain values):
//! - 0 `gain` (-60..12 dB, default 0): linear gain;
//! - 1 `close` (0..1): above 0.5 while the editor is shown, the plugin closes
//!   its (floating) window on its own, as a user clicking its close button.
//!
//! Editor (`gui` extension):
//! - X11 floating windows are "supported" without touching any display (the
//!   window is imaginary), so editor plumbing is testable headless; setting
//!   `CADENZA_TEST_GUI_EMBED=1` withdraws floating support, and the embedded
//!   mode then creates a real 300x200 X11 child window (needs a display).
//! - Showing the editor registers a 10 ms host timer (timer-support). On its
//!   first tick the plugin scripts a GUI edit: it asks for a parameter flush
//!   and outputs `gesture begin(0)`, `value(0, -6 dB)`, `gesture end(0)` from
//!   the next `process` or `params.flush`, and marks its state dirty. In the
//!   embedded mode the second tick asks the host to resize the editor to
//!   320x240. The X11 connection fd is watched through posix-fd-support.
//!
//! The state extension stores the gain.

use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use clack_extensions::audio_ports::{
	AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
	PluginAudioPortsImpl,
};
use clack_extensions::gui::{
	GuiApiType, GuiConfiguration, GuiSize, HostGui, PluginGui, PluginGuiImpl, Window,
};
use clack_extensions::params::{
	HostParams, ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter,
	PluginAudioProcessorParams, PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{HostState, PluginState, PluginStateImpl};
use clack_extensions::timer::{HostTimer, PluginTimer, PluginTimerImpl, TimerId};
use clack_plugin::entry::SinglePluginEntry;
use clack_plugin::events::Pckn;
use clack_plugin::events::event_types::{
	ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use clack_plugin::utils::Cookie;

const GAIN: u32 = 0;
const CLOSE: u32 = 1;
/// The value the scripted GUI edit sets `gain` to.
const SCRIPTED_GAIN: f64 = -6.0;

struct Spec {
	name: &'static str,
	min: f32,
	max: f32,
	default: f32,
}

const SPECS: [Spec; 2] = [
	Spec {
		name: "gain",
		min: -60.0,
		max: 12.0,
		default: 0.0,
	},
	Spec {
		name: "close",
		min: 0.0,
		max: 1.0,
		default: 0.0,
	},
];

pub struct Shared {
	values: [AtomicU32; 2],
	/// A scripted gesture waits to be output.
	script: AtomicBool,
}

impl PluginShared<'_> for Shared {}

impl Shared {
	fn get(&self, id: u32) -> f32 {
		f32::from_bits(self.values[id as usize].load(Ordering::Relaxed))
	}

	fn set(&self, id: u32, v: f64) {
		if let Some(spec) = SPECS.get(id as usize) {
			let v = v as f32;
			if !v.is_nan() {
				self.values[id as usize]
					.store(v.clamp(spec.min, spec.max).to_bits(), Ordering::Relaxed);
			}
		}
	}

	fn apply(&self, events: &InputEvents) {
		for e in events {
			if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event()
				&& let Some(id) = p.param_id()
			{
				self.set(id.get(), p.value());
			}
		}
	}

	/// Outputs the scripted GUI edit, once.
	fn output_script(&self, output: &mut OutputEvents) {
		if !self.script.swap(false, Ordering::AcqRel) {
			return;
		}
		let id = ClapId::new(GAIN);
		let _ = output.try_push(ParamGestureBeginEvent::new(0, id));
		let _ = output.try_push(ParamValueEvent::new(
			0,
			id,
			Pckn::match_all(),
			SCRIPTED_GAIN,
		));
		let _ = output.try_push(ParamGestureEndEvent::new(0, id));
		self.set(GAIN, SCRIPTED_GAIN);
	}
}

#[derive(Default)]
struct Gui {
	created: bool,
	embedded: bool,
	timer: Option<TimerId>,
	ticks: u32,
	size: (u32, u32),
	#[cfg(target_os = "linux")]
	x11: Option<x11::ChildWindow>,
}

pub struct MainThread<'a> {
	shared: &'a Shared,
	host: HostMainThreadHandle<'a>,
	gui: RefCell<Gui>,
	shown: Cell<bool>,
}

impl<'a> PluginMainThread<'a, Shared> for MainThread<'a> {}

impl PluginMainThreadParams for MainThread<'_> {
	fn count(&self) -> u32 {
		SPECS.len() as u32
	}

	fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
		let Some(s) = SPECS.get(index as usize) else {
			return;
		};
		info.set(&ParamInfo {
			id: ClapId::new(index),
			flags: ParamInfoFlags::IS_AUTOMATABLE,
			cookie: Cookie::empty(),
			name: s.name.as_bytes(),
			module: b"",
			min_value: s.min as f64,
			max_value: s.max as f64,
			default_value: s.default as f64,
		});
	}

	fn get_value(&self, id: ClapId) -> Option<f64> {
		((id.get() as usize) < SPECS.len()).then(|| self.shared.get(id.get()) as f64)
	}

	fn value_to_text(
		&self,
		_id: ClapId,
		value: f64,
		writer: &mut ParamDisplayWriter,
	) -> std::fmt::Result {
		use std::fmt::Write as _;
		write!(writer, "{value:.2}")
	}

	fn text_to_value(&self, _id: ClapId, text: &CStr) -> Option<f64> {
		text.to_str().ok()?.trim().parse().ok()
	}

	fn flush(&self, input: &InputEvents, output: &mut OutputEvents) {
		self.shared.apply(input);
		self.shared.output_script(output);
	}
}

impl PluginStateImpl for MainThread<'_> {
	fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
		output.write_all(b"CGUI")?;
		output.write_all(&self.shared.get(GAIN).to_le_bytes())?;
		Ok(())
	}

	fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
		let mut buf = [0u8; 8];
		input.read_exact(&mut buf)?;
		if &buf[..4] != b"CGUI" {
			return Err(PluginError::Message("not a GUI test plugin state"));
		}
		let gain = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
		self.shared.set(GAIN, gain as f64);
		Ok(())
	}
}

impl PluginAudioPortsImpl for MainThread<'_> {
	fn count(&self, _is_input: bool) -> u32 {
		1
	}

	fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
		if index == 0 {
			writer.set(&AudioPortInfo {
				id: ClapId::new(0),
				name: if is_input { b"Input" } else { b"Output" },
				channel_count: 2,
				flags: AudioPortFlags::IS_MAIN,
				port_type: Some(AudioPortType::STEREO),
				in_place_pair: Some(ClapId::new(0)),
			});
		}
	}
}

fn floating_allowed() -> bool {
	std::env::var_os("CADENZA_TEST_GUI_EMBED").is_none()
}

impl MainThread<'_> {
	fn unregister_timer(&self, gui: &mut Gui) {
		if let (Some(id), Some(timer)) = (gui.timer.take(), self.host.get_extension::<HostTimer>())
		{
			let _ = timer.unregister_timer(&self.host, id);
		}
	}
}

impl PluginGuiImpl for MainThread<'_> {
	fn is_api_supported(&self, configuration: GuiConfiguration) -> bool {
		// Exactly one of floating/embedded is "supported" at a time, so tests
		// control which one the host picks (now that it prefers embedding)
		// through the same env var: by default only the imaginary floating
		// window is offered (headless-testable); `CADENZA_TEST_GUI_EMBED=1`
		// withdraws it and offers the real embedded window instead.
		configuration.api_type == GuiApiType::X11 && configuration.is_floating == floating_allowed()
	}

	fn get_preferred_api(&self) -> Option<GuiConfiguration<'_>> {
		Some(GuiConfiguration {
			api_type: GuiApiType::X11,
			is_floating: floating_allowed(),
		})
	}

	fn create(&self, configuration: GuiConfiguration) -> Result<(), PluginError> {
		if !self.is_api_supported(configuration) {
			return Err(PluginError::Message("unsupported GUI API"));
		}
		let mut gui = self.gui.borrow_mut();
		*gui = Gui {
			created: true,
			embedded: !configuration.is_floating,
			size: (300, 200),
			..Gui::default()
		};
		Ok(())
	}

	fn destroy(&self) {
		let mut gui = self.gui.borrow_mut();
		self.unregister_timer(&mut gui);
		#[cfg(target_os = "linux")]
		if let Some(child) = gui.x11.take() {
			child.destroy(&self.host);
		}
		*gui = Gui::default();
		self.shown.set(false);
	}

	fn set_scale(&self, _scale: f64) -> Result<(), PluginError> {
		Ok(())
	}

	fn get_size(&self) -> Option<GuiSize> {
		let gui = self.gui.borrow();
		gui.created.then_some(GuiSize {
			width: gui.size.0,
			height: gui.size.1,
		})
	}

	fn can_resize(&self) -> bool {
		true
	}

	fn adjust_size(&self, size: GuiSize) -> Option<GuiSize> {
		Some(GuiSize {
			width: size.width.max(100),
			height: size.height.max(100),
		})
	}

	fn set_size(&self, size: GuiSize) -> Result<(), PluginError> {
		let mut gui = self.gui.borrow_mut();
		gui.size = (size.width, size.height);
		#[cfg(target_os = "linux")]
		if let Some(child) = &gui.x11 {
			child.resize(size.width, size.height);
		}
		Ok(())
	}

	fn set_parent(&self, window: Window) -> Result<(), PluginError> {
		#[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
		let mut gui = self.gui.borrow_mut();
		if !gui.embedded {
			return Err(PluginError::Message("not an embedded editor"));
		}
		#[cfg(target_os = "linux")]
		{
			let parent = window
				.as_x11_handle()
				.ok_or(PluginError::Message("not an X11 window"))?;
			let (width, height) = gui.size;
			gui.x11 = Some(
				x11::ChildWindow::create(parent as u32, width, height, &self.host)
					.map_err(|_| PluginError::Message("cannot create the X11 child window"))?,
			);
			Ok(())
		}
		#[cfg(not(target_os = "linux"))]
		{
			let _ = window;
			Err(PluginError::Message(
				"embedding is only implemented on Linux",
			))
		}
	}

	fn set_transient(&self, _window: Window) -> Result<(), PluginError> {
		Ok(())
	}

	fn show(&self) -> Result<(), PluginError> {
		let mut gui = self.gui.borrow_mut();
		if !gui.created {
			return Err(PluginError::Message("no GUI"));
		}
		if gui.timer.is_none() {
			let timer = self
				.host
				.get_extension::<HostTimer>()
				.ok_or(PluginError::Message("the host has no timer support"))?;
			gui.timer = Some(
				timer
					.register_timer(&self.host, 10)
					.map_err(|_| PluginError::Message("timer refused"))?,
			);
		}
		self.shown.set(true);
		Ok(())
	}

	fn hide(&self) -> Result<(), PluginError> {
		self.shown.set(false);
		Ok(())
	}
}

impl PluginTimerImpl for MainThread<'_> {
	fn on_timer(&self, _timer_id: TimerId) {
		if !self.shown.get() {
			return;
		}
		let (tick, embedded) = {
			let mut gui = self.gui.borrow_mut();
			gui.ticks += 1;
			(gui.ticks, gui.embedded)
		};
		if tick == 1 {
			self.shared.script.store(true, Ordering::Release);
			if let Some(params) = self.host.get_extension::<HostParams>() {
				params.request_flush(&self.host);
			}
			if let Some(state) = self.host.get_extension::<HostState>() {
				state.mark_dirty(&self.host);
			}
		}
		if tick == 2 && embedded {
			let size = GuiSize {
				width: 320,
				height: 240,
			};
			if let Some(gui) = self.host.get_extension::<HostGui>()
				&& gui
					.request_resize(&self.host, size.width, size.height)
					.is_ok()
			{
				let _ = self.set_size(size);
			}
		}
		if self.shared.get(CLOSE) > 0.5 && !embedded {
			self.shown.set(false);
			if let Some(gui) = self.host.get_extension::<HostGui>() {
				gui.closed(&self.host, false);
			}
		}
	}
}

#[cfg(target_os = "linux")]
impl clack_extensions::posix_fd::PluginPosixFdImpl for MainThread<'_> {
	fn on_fd(&self, _fd: std::os::fd::RawFd, _flags: clack_extensions::posix_fd::FdFlags) {
		if let Some(child) = &self.gui.borrow().x11 {
			child.drain_events();
		}
	}
}

#[cfg(target_os = "linux")]
mod x11 {
	use clack_extensions::posix_fd::{FdFlags, HostPosixFd};
	use clack_plugin::prelude::HostMainThreadHandle;
	use std::os::fd::AsRawFd;
	use x11rb::connection::Connection;
	use x11rb::protocol::xproto::{
		ConfigureWindowAux, ConnectionExt, CreateWindowAux, WindowClass,
	};
	use x11rb::rust_connection::RustConnection;

	/// The test editor: a plain child window on its own X connection.
	pub struct ChildWindow {
		conn: RustConnection,
		window: u32,
		fd: i32,
	}

	impl ChildWindow {
		pub fn create(
			parent: u32,
			width: u32,
			height: u32,
			host: &HostMainThreadHandle,
		) -> Result<Self, Box<dyn std::error::Error>> {
			let (conn, screen) = x11rb::connect(None)?;
			let visual = conn.setup().roots[screen].root_visual;
			let window = conn.generate_id()?;
			conn.create_window(
				x11rb::COPY_DEPTH_FROM_PARENT,
				window,
				parent,
				0,
				0,
				width as u16,
				height as u16,
				0,
				WindowClass::INPUT_OUTPUT,
				visual,
				&CreateWindowAux::new().background_pixel(0x0040_6080),
			)?;
			conn.map_window(window)?;
			conn.flush()?;
			let fd = conn.stream().as_raw_fd();
			if let Some(ext) = host.get_extension::<HostPosixFd>() {
				let _ = ext.register_fd(host, fd, FdFlags::READ);
			}
			Ok(Self { conn, window, fd })
		}

		pub fn resize(&self, width: u32, height: u32) {
			let _ = self.conn.configure_window(
				self.window,
				&ConfigureWindowAux::new().width(width).height(height),
			);
			let _ = self.conn.flush();
		}

		pub fn drain_events(&self) {
			while let Ok(Some(_)) = self.conn.poll_for_event() {}
		}

		pub fn destroy(self, host: &HostMainThreadHandle) {
			if let Some(ext) = host.get_extension::<HostPosixFd>() {
				let _ = ext.unregister_fd(host, self.fd);
			}
			let _ = self.conn.destroy_window(self.window);
			let _ = self.conn.flush();
		}
	}
}

pub struct GuiPlugin;

impl Plugin for GuiPlugin {
	type AudioProcessor<'a> = Processor<'a>;
	type Shared<'a> = Shared;
	type MainThread<'a> = MainThread<'a>;

	fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
		builder
			.register::<PluginAudioPorts>()
			.register::<PluginParams>()
			.register::<PluginState>()
			.register::<PluginGui>()
			.register::<PluginTimer>();
		#[cfg(target_os = "linux")]
		builder.register::<clack_extensions::posix_fd::PluginPosixFd>();
	}
}

impl DefaultPluginFactory for GuiPlugin {
	fn get_descriptor() -> PluginDescriptor {
		use clack_plugin::plugin::features::*;
		PluginDescriptor::new("dev.cadenza.test.gui", "Cadenza Test GUI")
			.with_vendor("Cadenza")
			.with_version("0.1.0")
			.with_description("Gain with a scripted editor (plugin editor test plugin).")
			.with_features([AUDIO_EFFECT, UTILITY, STEREO])
	}

	fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
		Ok(Shared {
			values: SPECS.map(|s| AtomicU32::new(s.default.to_bits())),
			script: AtomicBool::new(false),
		})
	}

	fn new_main_thread<'a>(
		host: HostMainThreadHandle<'a>,
		shared: &'a Shared,
	) -> Result<MainThread<'a>, PluginError> {
		Ok(MainThread {
			shared,
			host,
			gui: RefCell::new(Gui::default()),
			shown: Cell::new(false),
		})
	}
}

pub struct Processor<'a> {
	shared: &'a Shared,
}

impl<'a> PluginAudioProcessor<'a, Shared, MainThread<'a>> for Processor<'a> {
	fn activate(
		_host: HostAudioProcessorHandle<'a>,
		_main_thread: &MainThread<'a>,
		shared: &'a Shared,
		_audio_config: PluginAudioConfiguration,
	) -> Result<Self, PluginError> {
		Ok(Self { shared })
	}

	fn process(
		&mut self,
		_process: Process,
		mut audio: Audio,
		events: Events,
	) -> Result<ProcessStatus, PluginError> {
		self.shared.apply(events.input);
		self.shared.output_script(events.output);
		let gain = 10f32.powf(self.shared.get(GAIN) / 20.0);
		let frames = audio.frames_count() as usize;
		let Some(mut port) = audio.port_pair(0) else {
			return Ok(ProcessStatus::Continue);
		};
		let Some(mut channels) = port.channels()?.into_f32() else {
			return Ok(ProcessStatus::Continue);
		};
		for pair in channels.iter_mut() {
			match pair {
				ChannelPair::InPlace(buf) => buf.iter_mut().take(frames).for_each(|s| *s *= gain),
				ChannelPair::InputOutput(input, output) => {
					for (o, i) in output.iter_mut().zip(input.iter()).take(frames) {
						*o = i * gain;
					}
				}
				ChannelPair::OutputOnly(output) => output.iter_mut().for_each(|s| *s = 0.0),
				ChannelPair::InputOnly(_) => {}
			}
		}
		Ok(ProcessStatus::Continue)
	}
}

impl PluginAudioProcessorParams for Processor<'_> {
	fn flush(&mut self, input: &InputEvents, output: &mut OutputEvents) {
		self.shared.apply(input);
		self.shared.output_script(output);
	}
}

clack_export_entry!(SinglePluginEntry<GuiPlugin>);
