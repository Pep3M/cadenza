//! The on-screen keyboard shared by the instrument editors (Poly Synth, Electric
//! Piano): an octave picker, clickable keys (higher on the key = harder) and A–K on the
//! computer keyboard while the editor has the focus (Z / X octave), all through
//! [`AppState::preview_note_on`] / `preview_note_off`.

use crate::i18n::tr;
use cadenza_core::TrackId;
use egui::{
	Align2, CornerRadius, CursorIcon, Id, Key, Modifiers, Pos2, Rect, Sense, Ui, pos2, vec2,
};

use crate::state::AppState;
use crate::state::device_display::VoicePos;
use crate::state::poly_synth_editor as ps;
use crate::state::selection::DeviceRef;
use crate::theme;
use crate::ui::device_editor::BodyCx;

/// The keyboard strip along the bottom of an instrument dock.
pub const KEYS_H: f32 = 50.0;
const WHITE_W: f32 = 22.0;
const KEY_WHITE: egui::Color32 = egui::Color32::from_rgb(0xd9, 0xdb, 0xe0);
const KEY_BLACK: egui::Color32 = egui::Color32::from_rgb(0x1a, 0x1b, 0x1f);
const KEY_BLACK_LIT: egui::Color32 = egui::Color32::from_rgb(0x2c, 0x6f, 0xc0);
const KEY_LABEL: egui::Color32 = egui::Color32::from_rgb(0x55, 0x58, 0x5f);

/// Computer keys of the keyboard: A–K play, Z / X shift the octave.
const COMPUTER_KEYS: [(Key, char); 15] = [
	(Key::A, 'a'),
	(Key::W, 'w'),
	(Key::S, 's'),
	(Key::E, 'e'),
	(Key::D, 'd'),
	(Key::F, 'f'),
	(Key::T, 't'),
	(Key::G, 'g'),
	(Key::Y, 'y'),
	(Key::H, 'h'),
	(Key::U, 'u'),
	(Key::J, 'j'),
	(Key::K, 'k'),
	(Key::Z, 'z'),
	(Key::X, 'x'),
];

/// A note the editor holds: its track, pitch and when it started (for the voice dots).
#[derive(Debug, Clone, Copy)]
struct Held {
	track: TrackId,
	note: u8,
	since: f64,
}

/// The on-screen keyboard's octave and the notes the mouse / computer keys hold.
pub struct SynthKeys {
	base: u8,
	mouse: Option<Held>,
	computer: Vec<(Key, Held)>,
	/// The editor had the keyboard focus this frame ([`SynthKeys::shortcuts`] ran).
	focused: bool,
	device: Option<DeviceRef>,
}

impl Default for SynthKeys {
	fn default() -> Self {
		Self {
			base: ps::BASE_DEFAULT,
			mouse: None,
			computer: Vec::new(),
			focused: false,
			device: None,
		}
	}
}

impl SynthKeys {
	/// Releases every note held by the editor (editor closed or hidden, device changed).
	pub fn release(&mut self, state: &mut AppState) {
		if let Some(h) = self.mouse.take() {
			state.preview_note_off(h.track, h.note);
		}
		self.release_computer(state);
	}

	fn release_computer(&mut self, state: &mut AppState) {
		for (_, h) in self.computer.drain(..) {
			state.preview_note_off(h.track, h.note);
		}
	}

	/// The computer keyboard while the editor has the focus: A–K play (W E T Y U the
	/// black keys), released on key up; Z / X shift the octave.
	pub fn shortcuts(&mut self, ctx: &egui::Context, state: &mut AppState, r: DeviceRef) {
		let Some(track) = r.track else {
			return;
		};
		self.focused = true;
		let now = ctx.input(|i| i.time);
		for (key, c) in COMPUTER_KEYS {
			if !ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
				continue;
			}
			if let Some(octaves) = ps::octave_key(c) {
				self.base = ps::shift_base(self.base, octaves);
			} else if !self.computer.iter().any(|(k, _)| *k == key)
				&& let Some(note) = ps::computer_note(self.base, c)
			{
				state.preview_note_on(track, note, 0.8);
				self.computer.push((
					key,
					Held {
						track,
						note,
						since: now,
					},
				));
			}
		}
		let mut i = 0;
		while i < self.computer.len() {
			let (key, h) = self.computer[i];
			if ctx.input(|inp| inp.key_down(key)) {
				i += 1;
			} else {
				self.computer.remove(i);
				if !self.computer.iter().any(|(_, o)| o.note == h.note)
					&& self.mouse_note() != Some(h.note)
				{
					state.preview_note_off(h.track, h.note);
				}
			}
		}
	}

	fn mouse_note(&self) -> Option<u8> {
		self.mouse.map(|h| h.note)
	}

	/// Start of the body's frame: notes of another device go, computer notes go when the
	/// editor lost the focus, the mouse note goes on pointer release.
	pub fn begin(&mut self, ui: &Ui, state: &mut AppState, r: DeviceRef) {
		if self.device != Some(r) {
			self.release(state);
			self.device = Some(r);
		}
		if !self.focused {
			self.release_computer(state);
		}
		self.focused = false;
		if ui.input(|i| i.pointer.any_released())
			&& let Some(h) = self.mouse.take()
			&& !self.computer.iter().any(|(_, o)| o.note == h.note)
		{
			state.preview_note_off(h.track, h.note);
		}
	}

	/// The editor's held notes and how long they have sounded (ms), newest first.
	pub fn voices(&self, now: f64) -> Vec<VoicePos> {
		let mut held: Vec<f64> = self
			.computer
			.iter()
			.map(|(_, h)| h.since)
			.chain(self.mouse.map(|h| h.since))
			.collect();
		held.sort_by(|a, b| b.total_cmp(a));
		held.into_iter()
			.take(4)
			.map(|t| VoicePos::Held(((now - t) * 1000.0) as f32))
			.collect()
	}
}

pub fn keyboard_row(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	keys: &mut SynthKeys,
	rect: Rect,
	now: f64,
) {
	let cy = rect.center().y + 3.0;
	let picker = Rect::from_min_size(pos2(rect.left() + 2.0, cy - 12.0), vec2(84.0, 24.0));
	octave_picker(ui, cx, keys, picker);
	let room = rect.right() - picker.right() - 12.0;
	let octaves = ((room / (WHITE_W * 7.0)).floor() as usize).clamp(1, 3);
	let kb = Rect::from_min_size(
		pos2(picker.right() + 12.0, cy - 20.0),
		vec2(octaves as f32 * 7.0 * WHITE_W, 40.0),
	);
	keyboard(ui, state, cx, keys, kb, octaves, now);
	let hint = tr("Click keys or use A\u{2013}K on the computer keyboard \u{b7} Z / X octave");
	let g = ui
		.painter()
		.layout_no_wrap(hint.to_owned(), theme::medium(12.0), theme::TEXT_FAINT);
	if kb.right() + 12.0 + g.size().x < rect.right() {
		ui.painter().galley(
			pos2(kb.right() + 12.0, cy - g.size().y * 0.5),
			g,
			theme::TEXT_FAINT,
		);
	}
}

fn octave_picker(ui: &mut Ui, cx: &BodyCx, keys: &mut SynthKeys, rect: Rect) {
	let w = 22.0;
	let minus = Rect::from_min_size(rect.min, vec2(w, rect.height()));
	let plus = Rect::from_min_size(pos2(rect.right() - w, rect.top()), vec2(w, rect.height()));
	let mid = Rect::from_min_max(
		pos2(minus.right(), rect.top()),
		pos2(plus.left(), rect.bottom()),
	);
	for (r, glyph, delta, name, round) in [
		(
			minus,
			"\u{2212}",
			-1i8,
			tr("Octave down"),
			CornerRadius {
				nw: 4,
				sw: 4,
				ne: 0,
				se: 0,
			},
		),
		(
			plus,
			"+",
			1,
			tr("Octave up"),
			CornerRadius {
				nw: 0,
				sw: 0,
				ne: 4,
				se: 4,
			},
		),
	] {
		let resp = ui
			.interact(
				r,
				Id::new(("device-keyboard-octave", cx.salt, cx.r.device, delta)),
				Sense::click(),
			)
			.on_hover_cursor(CursorIcon::PointingHand);
		resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name));
		if resp.clicked() {
			keys.base = ps::shift_base(keys.base, delta);
		}
		let resp = resp.on_hover_text(format!("{name} (Z / X)"));
		ui.painter().rect_filled(
			r,
			round,
			if resp.hovered() {
				theme::BG_HOVER
			} else {
				theme::BG_WIDGET
			},
		);
		ui.painter().text(
			r.center(),
			Align2::CENTER_CENTER,
			glyph,
			theme::medium(13.0),
			theme::TEXT_DIM,
		);
	}
	ui.painter().rect_filled(mid, 0.0, theme::BG_DEEP);
	ui.painter().text(
		mid.center(),
		Align2::CENTER_CENTER,
		ps::note_name(keys.base),
		theme::mono(11.5),
		theme::TEXT,
	);
}

#[allow(clippy::too_many_arguments)]
fn keyboard(
	ui: &mut Ui,
	state: &mut AppState,
	cx: &BodyCx,
	keys: &mut SynthKeys,
	rect: Rect,
	octaves: usize,
	now: f64,
) {
	let resp = ui.interact(
		rect,
		Id::new(("device-keyboard", cx.salt, cx.r.device)),
		Sense::click_and_drag(),
	);
	resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, tr("Keyboard")));
	let at = |pos: Pos2| {
		ps::key_at(
			keys.base,
			octaves,
			(pos.x - rect.left()) / WHITE_W,
			(pos.y - rect.top()) / rect.height(),
		)
	};
	if let Some(track) = cx.r.track
		&& resp.is_pointer_button_down_on()
		&& let Some(pos) = ui.ctx().pointer_interact_pos()
		&& let Some(note) = at(pos)
		&& keys.mouse_note() != Some(note)
	{
		if let Some(old) = keys.mouse.take()
			&& !keys.computer.iter().any(|(_, h)| h.note == old.note)
		{
			state.preview_note_off(old.track, old.note);
		}
		let y = ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
		state.preview_note_on(track, note, 0.35 + 0.65 * y);
		keys.mouse = Some(Held {
			track,
			note,
			since: now,
		});
	}
	let _ = resp.on_hover_cursor(CursorIcon::PointingHand);
	if !ui.is_rect_visible(rect) {
		return;
	}
	let live = state.live_voices(cx.r.device);
	if !live.is_idle() {
		ui.ctx().request_repaint();
	}
	let lit = state.lit_keys(cx.r, &live);
	let p = ui.painter();
	for (i, note) in ps::white_keys(keys.base, octaves).into_iter().enumerate() {
		let k = Rect::from_min_size(
			pos2(rect.left() + i as f32 * WHITE_W, rect.top()),
			vec2(WHITE_W - 1.0, rect.height()),
		);
		let on = lit.contains(&note);
		p.rect_filled(
			k,
			CornerRadius {
				nw: 0,
				ne: 0,
				sw: 3,
				se: 3,
			},
			if on { theme::ACCENT } else { KEY_WHITE },
		);
		if note % 12 == 0 {
			p.text(
				pos2(k.center().x, k.bottom() - 7.0),
				Align2::CENTER_CENTER,
				ps::note_name(note),
				theme::medium(9.5),
				if on { theme::TEXT_ON_ACCENT } else { KEY_LABEL },
			);
		}
	}
	for (note, left) in ps::black_keys(keys.base, octaves) {
		let k = Rect::from_min_size(
			pos2(rect.left() + left * WHITE_W, rect.top()),
			vec2(ps::BLACK_WIDTH * WHITE_W, rect.height() * ps::BLACK_DEPTH),
		);
		p.rect_filled(
			k,
			CornerRadius {
				nw: 0,
				ne: 0,
				sw: 2,
				se: 2,
			},
			if lit.contains(&note) {
				KEY_BLACK_LIT
			} else {
				KEY_BLACK
			},
		);
	}
}
