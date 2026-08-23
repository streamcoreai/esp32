//! ST7789 240x280 TFT display driver. Renders at ~10 FPS on a dedicated thread.

use alloc::string::String;
use alloc::vec::Vec;

use display_interface_spi::SPIInterface;
use embedded_graphics::mono_font::ascii::{FONT_10X20, FONT_9X18_BOLD};
use embedded_graphics::mono_font::{MonoTextStyle, MonoTextStyleBuilder};
use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::*;
use embedded_graphics::primitives::{Circle, PrimitiveStyle, Rectangle};
use embedded_graphics::text::Text;
use esp_idf_svc::hal::gpio::{AnyIOPin, PinDriver};
use esp_idf_svc::hal::spi::config::Config as SpiConfig;
use esp_idf_svc::hal::spi::{SpiDeviceDriver, SpiDriver, SpiDriverConfig, SPI2};
use esp_idf_svc::hal::units::Hertz;
use log::info;
use mipidsi::models::ST7789;
use mipidsi::options::{ColorInversion, ColorOrder, Orientation, Rotation};
use mipidsi::Builder;

const WIDTH: u32 = 240;
const HEIGHT: u32 = 280;

const STATUS_Y: i32 = 50; // connection + mic label
const VU_Y: i32 = 80; // VU meter
const VU_H: u32 = 14;
const DOTS_Y: i32 = 106; // speaking dots
const TRANSCRIPT_Y: i32 = 130; // transcript starts here
const TRANSCRIPT_BOTTOM: i32 = 270;

const BG: Rgb565 = Rgb565::BLACK;
const CYAN: Rgb565 = Rgb565::new(0, 63, 31);
const WHITE: Rgb565 = Rgb565::WHITE;
const GREEN: Rgb565 = Rgb565::new(0, 63, 0);
const RED: Rgb565 = Rgb565::new(31, 0, 0);
const VU_GREEN: Rgb565 = Rgb565::new(4, 50, 4);
const VU_BG: Rgb565 = Rgb565::new(3, 6, 3);
const DIM_CYAN: Rgb565 = Rgb565::new(0, 20, 10);

#[derive(Clone, Copy, PartialEq)]
pub enum Role {
    User,
    Assistant,
}

pub struct DisplayState {
    pub mic_muted: bool,
    pub connected: bool,
    pub audio_level: u16,
    pub speaking: bool,
    pub transcript_lines: Vec<(Role, String)>,
    pub dirty: u32,
}

impl DisplayState {
    pub fn new() -> Self {
        Self {
            mic_muted: true,
            connected: false,
            audio_level: 0,
            speaking: false,
            transcript_lines: Vec::new(),
            dirty: 0,
        }
    }

    pub fn push_transcript(&mut self, role: Role, text: String) {
        self.transcript_lines.push((role, text));
        if self.transcript_lines.len() > 20 {
            self.transcript_lines.remove(0);
        }
        self.dirty = self.dirty.wrapping_add(1);
    }

    pub fn set_last_or_push(&mut self, role: Role, text: String) {
        if let Some(last) = self.transcript_lines.last_mut() {
            if last.0 == role {
                last.1 = text;
                self.dirty = self.dirty.wrapping_add(1);
                return;
            }
        }
        self.transcript_lines.push((role, text));
        if self.transcript_lines.len() > 20 {
            self.transcript_lines.remove(0);
        }
        self.dirty = self.dirty.wrapping_add(1);
    }

    pub fn append_or_push(&mut self, role: Role, text: &str) {
        if let Some(last) = self.transcript_lines.last_mut() {
            if last.0 == role {
                last.1.push_str(text);
                self.dirty = self.dirty.wrapping_add(1);
                return;
            }
        }
        self.transcript_lines
            .push((role, alloc::string::String::from(text)));
        if self.transcript_lines.len() > 20 {
            self.transcript_lines.remove(0);
        }
        self.dirty = self.dirty.wrapping_add(1);
    }
}

fn render_wrapped<D: DrawTarget<Color = Rgb565>>(
    text: &str,
    start: Point,
    max_chars: usize,
    line_height: i32,
    y_limit: i32,
    style: MonoTextStyle<'_, Rgb565>,
    display: &mut D,
) -> i32 {
    let mut y = start.y;
    let x = start.x;
    let mut pos = 0;
    while pos < text.len() && y <= y_limit - line_height {
        let remaining = &text[pos..];
        let chunk_len = remaining.len().min(max_chars);
        let break_at = if chunk_len < remaining.len() {
            remaining[..chunk_len]
                .rfind(' ')
                .map(|s| if s > 0 { s } else { chunk_len })
                .unwrap_or(chunk_len)
        } else {
            chunk_len
        };
        let chunk = &remaining[..break_at];
        let _ = Text::new(chunk, Point::new(x, y), style).draw(display);
        y += line_height;
        pos += break_at;
        if pos < text.len() && text.as_bytes()[pos] == b' ' {
            pos += 1;
        }
    }
    y
}

pub fn display_thread(
    spi2: SPI2<'static>,
    mosi: AnyIOPin<'static>,
    sclk: AnyIOPin<'static>,
    cs: AnyIOPin<'static>,
    dc: AnyIOPin<'static>,
    backlight: AnyIOPin<'static>,
    state: alloc::sync::Arc<spin::Mutex<DisplayState>>,
) {
    if let Err(e) = display_thread_inner(spi2, mosi, sclk, dc, cs, backlight, state) {
        log::error!("Display thread crashed: {e}");
    }
}

fn display_thread_inner(
    spi2: SPI2<'static>,
    mosi: AnyIOPin<'static>,
    sclk: AnyIOPin<'static>,
    dc: AnyIOPin<'static>,
    cs: AnyIOPin<'static>,
    backlight: AnyIOPin<'static>,
    state: alloc::sync::Arc<spin::Mutex<DisplayState>>,
) -> anyhow::Result<()> {
    info!("Display: initialising SPI + ST7789...");

    let mut bl = PinDriver::output(backlight)?;
    bl.set_high()?;

    let spi_driver = SpiDriver::new(
        spi2,
        sclk,
        mosi,
        None::<AnyIOPin>,
        &SpiDriverConfig::default(),
    )?;

    let spi_config = SpiConfig::default().baudrate(Hertz(40_000_000));

    let spi_device = SpiDeviceDriver::new(spi_driver, Some(cs), &spi_config)?;

    let dc_pin = PinDriver::output(dc)?;
    let spi_iface = SPIInterface::new(spi_device, dc_pin);

    let mut display = Builder::new(ST7789, spi_iface)
        .display_size(WIDTH as u16, HEIGHT as u16)
        .display_offset(0, 20)
        .color_order(ColorOrder::Rgb)
        .invert_colors(ColorInversion::Inverted)
        .orientation(Orientation::new().rotate(Rotation::Deg0))
        .init(&mut esp_idf_svc::hal::delay::Ets)
        .map_err(|_| anyhow::anyhow!("ST7789 init failed"))?;

    display
        .clear(BG)
        .map_err(|_| anyhow::anyhow!("clear failed"))?;

    info!("Display: ST7789 ready ({}x{})", WIDTH, HEIGHT);

    let _bold = MonoTextStyle::new(&FONT_9X18_BOLD, WHITE);
    let text_white = MonoTextStyle::new(&FONT_10X20, WHITE);
    let text_cyan = MonoTextStyle::new(&FONT_10X20, CYAN);

    let mut frame: u32 = 0;
    let mut last_dirty: u32 = 0;
    let mut force_redraw = true;
    let mut prev_connected = false;
    let mut prev_mic_muted = true;
    let mut prev_speaking = false;

    loop {
        frame = frame.wrapping_add(1);

        let (mic_muted, connected, audio_level, speaking, dirty, transcript_snapshot) = {
            let s = state.lock();
            let tsnap = if s.dirty != last_dirty || force_redraw {
                Some(s.transcript_lines.clone())
            } else {
                None
            };
            (
                s.mic_muted,
                s.connected,
                s.audio_level,
                s.speaking,
                s.dirty,
                tsnap,
            )
        };

        let status_changed =
            connected != prev_connected || mic_muted != prev_mic_muted || force_redraw;
        if status_changed {
            prev_connected = connected;
            prev_mic_muted = mic_muted;

            let conn_color = if connected { GREEN } else { RED };
            let mic_color = if mic_muted { RED } else { GREEN };
            let conn_text = if connected { "Online " } else { "Offline" };
            let mic_text = if mic_muted { "MUTED" } else { "LIVE " };

            let cx = (WIDTH / 2) as i32;

            Circle::new(Point::new(cx - 85, STATUS_Y - 8), 16)
                .into_styled(PrimitiveStyle::with_fill(conn_color))
                .draw(&mut display)
                .ok();

            let conn_style = MonoTextStyleBuilder::new()
                .font(&FONT_9X18_BOLD)
                .text_color(WHITE)
                .background_color(BG)
                .build();
            Text::new(conn_text, Point::new(cx - 64, STATUS_Y + 6), conn_style)
                .draw(&mut display)
                .ok();

            let mic_style = MonoTextStyleBuilder::new()
                .font(&FONT_9X18_BOLD)
                .text_color(mic_color)
                .background_color(BG)
                .build();
            Text::new(mic_text, Point::new(cx + 16, STATUS_Y + 6), mic_style)
                .draw(&mut display)
                .ok();
        }

        let vu_width: u32 = 200;
        let vu_x = ((WIDTH - vu_width) / 2) as i32;
        let level_frac = if audio_level > 0 {
            let normalized = (audio_level as f32 / 32767.0).min(1.0);
            let curved = (normalized * 3.0).min(1.0);
            (curved * vu_width as f32) as u32
        } else {
            0
        };
        if level_frac > 0 {
            Rectangle::new(Point::new(vu_x, VU_Y), Size::new(level_frac, VU_H))
                .into_styled(PrimitiveStyle::with_fill(VU_GREEN))
                .draw(&mut display)
                .ok();
        }
        if level_frac < vu_width {
            Rectangle::new(
                Point::new(vu_x + level_frac as i32, VU_Y),
                Size::new(vu_width - level_frac, VU_H),
            )
            .into_styled(PrimitiveStyle::with_fill(VU_BG))
            .draw(&mut display)
            .ok();
        }

        if speaking != prev_speaking && !speaking {
            Rectangle::new(Point::new(0, DOTS_Y - 2), Size::new(WIDTH, 20))
                .into_styled(PrimitiveStyle::with_fill(BG))
                .draw(&mut display)
                .ok();
        }
        prev_speaking = speaking;

        if speaking {
            let phase = frame % 30;
            for i in 0..3u32 {
                let dot_phase = (phase + i * 10) % 30;
                let bright = dot_phase < 15;
                let color = if bright { CYAN } else { DIM_CYAN };
                let x = (WIDTH / 2) as i32 - 26 + (i as i32 * 22);
                Circle::new(Point::new(x, DOTS_Y), 12)
                    .into_styled(PrimitiveStyle::with_fill(color))
                    .draw(&mut display)
                    .ok();
            }
        }

        if let Some(lines) = transcript_snapshot {
            Rectangle::new(
                Point::new(0, TRANSCRIPT_Y),
                Size::new(WIDTH, (TRANSCRIPT_BOTTOM - TRANSCRIPT_Y) as u32),
            )
            .into_styled(PrimitiveStyle::with_fill(BG))
            .draw(&mut display)
            .ok();

            let max_chars_per_line = (WIDTH as usize - 16) / 10; // FONT_10X20 is 10px wide
            let line_height = 22i32;

            let last_user = lines.iter().rev().find(|(r, _)| *r == Role::User);
            let last_ai = lines.iter().rev().find(|(r, _)| *r == Role::Assistant);

            let mut y = TRANSCRIPT_Y + 16;

            if let Some((_, text)) = last_user {
                let full = alloc::format!("You: {}", text);
                y = render_wrapped(
                    &full,
                    Point::new(8, y),
                    max_chars_per_line,
                    line_height,
                    TRANSCRIPT_BOTTOM,
                    text_cyan,
                    &mut display,
                );
                y += 8;
            }

            if let Some((_, text)) = last_ai {
                let full = alloc::format!("AI: {}", text);
                render_wrapped(
                    &full,
                    Point::new(8, y),
                    max_chars_per_line,
                    line_height,
                    TRANSCRIPT_BOTTOM,
                    text_white,
                    &mut display,
                );
            }

            last_dirty = dirty;
            force_redraw = false;
        }

        std::thread::sleep(core::time::Duration::from_millis(100));
    }
}

// ---------------------------------------------------------------------------
// High-level handle — spawns the thread and exposes typed updaters.
// ---------------------------------------------------------------------------

/// Live handle to the on-board ST7789 display. Construct with [`spawn`] and
/// drive from your [`Agent`](crate::Agent) callbacks.
///
/// ```ignore
/// let display = va::display::DisplayHandle::spawn(
///     p.spi2,
///     p.pins.gpio47.downgrade(),
///     p.pins.gpio21.downgrade(),
///     p.pins.gpio14.downgrade(),
///     p.pins.gpio45.downgrade(),
///     p.pins.gpio48.downgrade(),
/// )?;
///
/// agent.connect_with_callbacks(va::AgentOptions {
///     on_state_changed: {
///         let d = display.clone();
///         Some(Box::new(move |s| d.set_connected(s == va::ConnectionState::Connected)))
///     },
///     on_transcript: {
///         let d = display.clone();
///         Some(Box::new(move |t, _| d.push_user_text(t.into())))
///     },
///     ..Default::default()
/// });
/// ```
#[derive(Clone)]
pub struct DisplayHandle {
    state: alloc::sync::Arc<spin::Mutex<DisplayState>>,
}

impl DisplayHandle {
    /// Spawn the background rendering thread and return a handle.
    pub fn spawn(
        spi2: SPI2<'static>,
        mosi: AnyIOPin<'static>,
        sclk: AnyIOPin<'static>,
        cs: AnyIOPin<'static>,
        dc: AnyIOPin<'static>,
        backlight: AnyIOPin<'static>,
    ) -> anyhow::Result<Self> {
        let state = alloc::sync::Arc::new(spin::Mutex::new(DisplayState::new()));
        let st = state.clone();
        std::thread::Builder::new()
            .name("voiceagent-display".into())
            .stack_size(32 * 1024)
            .spawn(move || display_thread(spi2, mosi, sclk, cs, dc, backlight, st))?;
        Ok(Self { state })
    }

    pub fn set_connected(&self, connected: bool) {
        self.state.lock().connected = connected;
    }

    pub fn set_mic_muted(&self, muted: bool) {
        self.state.lock().mic_muted = muted;
    }

    pub fn set_speaking(&self, speaking: bool) {
        self.state.lock().speaking = speaking;
    }

    pub fn set_audio_level(&self, level: u16) {
        self.state.lock().audio_level = level;
    }

    /// Replace the most-recent user transcript line (for partial updates),
    /// or push a new one if the last line was from the assistant.
    pub fn set_user_text(&self, text: alloc::string::String) {
        self.state.lock().set_last_or_push(Role::User, text);
    }

    /// Append to the most-recent assistant line, or push a new one if the
    /// last line was from the user.
    pub fn append_assistant_text(&self, text: &str) {
        self.state.lock().append_or_push(Role::Assistant, text);
    }

    /// Force a brand-new user transcript line (use for `is_final = true`).
    pub fn push_user_final(&self, text: alloc::string::String) {
        self.state.lock().push_transcript(Role::User, text);
    }
}
