//! Allocation-free async SSD1680 support for the DEPG0290BNS800F6 panel.
//!
//! The driver owns the panel's SPI device, data/command pin, reset pin,
//! caller-bounded BUSY waiter, and delay source. It deliberately implements
//! only the conservative full-frame lifecycle required by the Heltec Vision
//! Master E290: initialize, write and activate one frame, then enter deep
//! sleep.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::convert::Infallible;

use embedded_graphics_core::{
    Pixel,
    draw_target::DrawTarget,
    geometry::{OriginDimensions, Size},
    pixelcolor::BinaryColor,
    prelude::Point,
};
use embedded_hal::digital::OutputPin;
use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};

/// Landscape pixel width of the DEPG0290BNS800F6 panel fitted to the E290.
pub const E290_FRAME_WIDTH: usize = 296;
/// Landscape pixel height of the DEPG0290BNS800F6 panel fitted to the E290.
pub const E290_FRAME_HEIGHT: usize = 128;
/// Bytes in one controller column of the E290 panel.
pub const E290_BYTES_PER_COLUMN: usize = E290_FRAME_HEIGHT / 8;
/// Exact byte count of one monochrome E290 full frame.
pub const E290_FRAME_BYTES: usize = E290_FRAME_WIDTH * E290_BYTES_PER_COLUMN;

const SOFTWARE_RESET: u8 = 0x12;
const WRITE_BLACK_WHITE_RAM: u8 = 0x24;
const MASTER_ACTIVATION: u8 = 0x20;
const DEEP_SLEEP_MODE: u8 = 0x10;
const DEEP_SLEEP_MODE_1: u8 = 0x01;

const POWER_SETTLE_MS: u32 = 10;
const RESET_PULSE_US: u32 = 200;

const _: () = assert!(E290_FRAME_HEIGHT.is_multiple_of(8));
const _: () = assert!(E290_FRAME_BYTES == 4_736);

/// A fixed, allocation-free landscape framebuffer for the E290 panel.
///
/// Bytes use SSD1680 controller stream order: `x * 16 + y / 8`. Within each
/// byte the most significant bit is the uppermost pixel. A set bit is white
/// and a cleared bit is black.
pub struct E290FrameBuffer {
    bytes: [u8; E290_FRAME_BYTES],
}

impl E290FrameBuffer {
    /// Construct an entirely white frame.
    pub const fn new_white() -> Self {
        Self {
            bytes: [u8::MAX; E290_FRAME_BYTES],
        }
    }

    /// Fill the complete frame with one binary color.
    pub fn clear(&mut self, color: BinaryColor) {
        self.bytes.fill(byte_for_color(color));
    }

    /// Borrow the exact controller-stream bytes.
    pub const fn as_bytes(&self) -> &[u8; E290_FRAME_BYTES] {
        &self.bytes
    }

    fn set_pixel(&mut self, point: Point, color: BinaryColor) {
        let Ok(x) = usize::try_from(point.x) else {
            return;
        };
        let Ok(y) = usize::try_from(point.y) else {
            return;
        };
        if x >= E290_FRAME_WIDTH || y >= E290_FRAME_HEIGHT {
            return;
        }

        let byte_index = x * E290_BYTES_PER_COLUMN + y / 8;
        let bit_mask = 1 << (7 - y % 8);
        match color {
            BinaryColor::Off => self.bytes[byte_index] &= !bit_mask,
            BinaryColor::On => self.bytes[byte_index] |= bit_mask,
        }
    }
}

impl Default for E290FrameBuffer {
    fn default() -> Self {
        Self::new_white()
    }
}

impl OriginDimensions for E290FrameBuffer {
    fn size(&self) -> Size {
        Size::new(E290_FRAME_WIDTH as u32, E290_FRAME_HEIGHT as u32)
    }
}

impl DrawTarget for E290FrameBuffer {
    type Color = BinaryColor;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        for Pixel(point, color) in pixels {
            self.set_pixel(point, color);
        }
        Ok(())
    }

    fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
        E290FrameBuffer::clear(self, color);
        Ok(())
    }
}

const fn byte_for_color(color: BinaryColor) -> u8 {
    match color {
        BinaryColor::Off => 0,
        BinaryColor::On => u8::MAX,
    }
}

/// One SSD1680 operation failed.
///
/// Each variant retains the concrete source error without adding `Debug` or
/// `Display` formatting that could expose implementation-owned state.
pub enum Ssd1680Error<SpiError, DcError, ResetError, BusyError> {
    /// An SPI transaction failed.
    Spi(SpiError),
    /// Driving the data/command pin failed.
    DataCommand(DcError),
    /// Driving the hardware reset pin failed.
    Reset(ResetError),
    /// The caller-bounded BUSY wait failed.
    Busy(BusyError),
}

/// Async full-frame SSD1680 owner for an E290 e-paper panel.
pub struct Ssd1680<SPI, DC, RESET, BUSY, DELAY> {
    spi: SPI,
    dc: DC,
    reset: RESET,
    busy: BUSY,
    delay: DELAY,
}

impl<SPI, DC, RESET, BUSY, DELAY> Ssd1680<SPI, DC, RESET, BUSY, DELAY> {
    /// Construct a driver from its sole hardware owners.
    pub const fn new(spi: SPI, dc: DC, reset: RESET, busy: BUSY, delay: DELAY) -> Self {
        Self {
            spi,
            dc,
            reset,
            busy,
            delay,
        }
    }

    /// Release every owned hardware resource.
    pub fn release(self) -> (SPI, DC, RESET, BUSY, DELAY) {
        (self.spi, self.dc, self.reset, self.busy, self.delay)
    }
}

impl<SPI, DC, RESET, BUSY, DELAY> Ssd1680<SPI, DC, RESET, BUSY, DELAY>
where
    SPI: SpiDevice<u8>,
    DC: OutputPin,
    RESET: OutputPin,
    BUSY: Wait,
    DELAY: DelayNs,
{
    /// Settle panel power, pulse hardware reset, and issue software reset.
    ///
    /// BUSY waits are delegated directly to the supplied [`Wait`]
    /// implementation. The board integration is responsible for bounding
    /// them.
    pub async fn initialize(
        &mut self,
    ) -> Result<(), Ssd1680Error<SPI::Error, DC::Error, RESET::Error, BUSY::Error>> {
        self.delay.delay_ms(POWER_SETTLE_MS).await;
        self.reset.set_low().map_err(Ssd1680Error::Reset)?;
        self.delay.delay_us(RESET_PULSE_US).await;
        self.reset.set_high().map_err(Ssd1680Error::Reset)?;
        self.delay.delay_us(RESET_PULSE_US).await;
        self.wait_until_idle().await?;

        self.write_command(SOFTWARE_RESET).await?;
        self.wait_until_idle().await
    }

    /// Wait for an idle controller, then write and activate one complete E290
    /// frame.
    pub async fn refresh(
        &mut self,
        frame: &E290FrameBuffer,
    ) -> Result<(), Ssd1680Error<SPI::Error, DC::Error, RESET::Error, BUSY::Error>> {
        self.wait_until_idle().await?;
        self.write_command(WRITE_BLACK_WHITE_RAM).await?;
        self.write_data(frame.as_bytes()).await?;
        self.write_command(MASTER_ACTIVATION).await?;
        self.wait_until_idle().await
    }

    /// Wait for an idle controller, then enter SSD1680 deep-sleep mode 1.
    ///
    /// Hardware reset is required before any subsequent command.
    pub async fn deep_sleep(
        &mut self,
    ) -> Result<(), Ssd1680Error<SPI::Error, DC::Error, RESET::Error, BUSY::Error>> {
        self.wait_until_idle().await?;
        self.write_command(DEEP_SLEEP_MODE).await?;
        self.write_data(&[DEEP_SLEEP_MODE_1]).await
    }

    async fn write_command(
        &mut self,
        command: u8,
    ) -> Result<(), Ssd1680Error<SPI::Error, DC::Error, RESET::Error, BUSY::Error>> {
        self.dc.set_low().map_err(Ssd1680Error::DataCommand)?;
        self.spi.write(&[command]).await.map_err(Ssd1680Error::Spi)
    }

    async fn write_data(
        &mut self,
        data: &[u8],
    ) -> Result<(), Ssd1680Error<SPI::Error, DC::Error, RESET::Error, BUSY::Error>> {
        self.dc.set_high().map_err(Ssd1680Error::DataCommand)?;
        self.spi.write(data).await.map_err(Ssd1680Error::Spi)
    }

    async fn wait_until_idle(
        &mut self,
    ) -> Result<(), Ssd1680Error<SPI::Error, DC::Error, RESET::Error, BUSY::Error>> {
        self.busy.wait_for_low().await.map_err(Ssd1680Error::Busy)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        convert::Infallible,
        future::Future,
        pin::pin,
        task::{Context, Poll, Waker},
    };
    use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

    use embedded_graphics_core::{Pixel, draw_target::DrawTarget, prelude::Point};
    use embedded_hal::{
        digital::{ErrorType as DigitalErrorType, OutputPin},
        spi::{ErrorType as SpiErrorType, Operation, SpiDevice as BlockingSpiDevice},
    };
    use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};

    use super::{
        E290_BYTES_PER_COLUMN, E290_FRAME_BYTES, E290_FRAME_HEIGHT, E290_FRAME_WIDTH,
        E290FrameBuffer, Ssd1680, Ssd1680Error,
    };
    use embedded_graphics_core::pixelcolor::BinaryColor;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Event {
        Spi(Vec<u8>),
        DcLow,
        DcHigh,
        ResetLow,
        ResetHigh,
        BusyLow,
        BusyTimeout,
        DelayNs(u32),
    }

    type Events = Rc<RefCell<Vec<Event>>>;

    struct MockSpi {
        events: Events,
    }

    impl SpiErrorType for MockSpi {
        type Error = Infallible;
    }

    impl SpiDevice<u8> for MockSpi {
        async fn transaction(
            &mut self,
            operations: &mut [Operation<'_, u8>],
        ) -> Result<(), Self::Error> {
            for operation in operations {
                match operation {
                    Operation::Write(bytes) => {
                        self.events.borrow_mut().push(Event::Spi(bytes.to_vec()));
                    }
                    _ => panic!("driver issued a non-write SPI operation"),
                }
            }
            Ok(())
        }
    }

    struct MockOutput {
        events: Events,
        low: Event,
        high: Event,
    }

    impl DigitalErrorType for MockOutput {
        type Error = Infallible;
    }

    impl OutputPin for MockOutput {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.events.borrow_mut().push(self.low.clone());
            Ok(())
        }

        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.events.borrow_mut().push(self.high.clone());
            Ok(())
        }
    }

    struct MockBusy {
        events: Events,
        fail_on_wait: Option<usize>,
        waits: usize,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct MockBusyError;

    impl embedded_hal::digital::Error for MockBusyError {
        fn kind(&self) -> embedded_hal::digital::ErrorKind {
            embedded_hal::digital::ErrorKind::Other
        }
    }

    impl DigitalErrorType for MockBusy {
        type Error = MockBusyError;
    }

    impl Wait for MockBusy {
        async fn wait_for_high(&mut self) -> Result<(), Self::Error> {
            panic!("driver waited for BUSY high")
        }

        async fn wait_for_low(&mut self) -> Result<(), Self::Error> {
            let wait = self.waits;
            self.waits += 1;
            if self.fail_on_wait == Some(wait) {
                self.events.borrow_mut().push(Event::BusyTimeout);
                Err(MockBusyError)
            } else {
                self.events.borrow_mut().push(Event::BusyLow);
                Ok(())
            }
        }

        async fn wait_for_rising_edge(&mut self) -> Result<(), Self::Error> {
            panic!("driver waited for a BUSY edge")
        }

        async fn wait_for_falling_edge(&mut self) -> Result<(), Self::Error> {
            panic!("driver waited for a BUSY edge")
        }

        async fn wait_for_any_edge(&mut self) -> Result<(), Self::Error> {
            panic!("driver waited for a BUSY edge")
        }
    }

    struct MockDelay {
        events: Events,
    }

    impl DelayNs for MockDelay {
        async fn delay_ns(&mut self, ns: u32) {
            self.events.borrow_mut().push(Event::DelayNs(ns));
        }
    }

    type MockDriver = Ssd1680<MockSpi, MockOutput, MockOutput, MockBusy, MockDelay>;

    fn mock_driver(events: &Events, fail_on_wait: Option<usize>) -> MockDriver {
        Ssd1680::new(
            MockSpi {
                events: Rc::clone(events),
            },
            MockOutput {
                events: Rc::clone(events),
                low: Event::DcLow,
                high: Event::DcHigh,
            },
            MockOutput {
                events: Rc::clone(events),
                low: Event::ResetLow,
                high: Event::ResetHigh,
            },
            MockBusy {
                events: Rc::clone(events),
                fail_on_wait,
                waits: 0,
            },
            MockDelay {
                events: Rc::clone(events),
            },
        )
    }

    #[test]
    fn exact_full_frame_lifecycle_sequence() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = mock_driver(&events, None);
        let mut frame = E290FrameBuffer::new_white();
        frame
            .draw_iter([
                Pixel(Point::new(0, 0), BinaryColor::Off),
                Pixel(Point::new(295, 127), BinaryColor::Off),
            ])
            .unwrap();

        assert!(run(driver.initialize()).is_ok());
        assert!(run(driver.refresh(&frame)).is_ok());
        assert!(run(driver.deep_sleep()).is_ok());

        let expected = vec![
            Event::DelayNs(10_000_000),
            Event::ResetLow,
            Event::DelayNs(200_000),
            Event::ResetHigh,
            Event::DelayNs(200_000),
            Event::BusyLow,
            Event::DcLow,
            Event::Spi(vec![0x12]),
            Event::BusyLow,
            Event::BusyLow,
            Event::DcLow,
            Event::Spi(vec![0x24]),
            Event::DcHigh,
            Event::Spi(frame.as_bytes().to_vec()),
            Event::DcLow,
            Event::Spi(vec![0x20]),
            Event::BusyLow,
            Event::BusyLow,
            Event::DcLow,
            Event::Spi(vec![0x10]),
            Event::DcHigh,
            Event::Spi(vec![0x01]),
        ];
        assert_eq!(*events.borrow(), expected);
    }

    #[test]
    fn refresh_busy_timeout_prevents_every_command_and_frame_write() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = mock_driver(&events, Some(0));
        let frame = E290FrameBuffer::new_white();

        assert!(matches!(
            run(driver.refresh(&frame)),
            Err(Ssd1680Error::Busy(MockBusyError))
        ));
        assert_eq!(*events.borrow(), vec![Event::BusyTimeout]);
    }

    #[test]
    fn deep_sleep_busy_timeout_prevents_every_command() {
        let events = Rc::new(RefCell::new(Vec::new()));
        let mut driver = mock_driver(&events, Some(0));

        assert!(matches!(
            run(driver.deep_sleep()),
            Err(Ssd1680Error::Busy(MockBusyError))
        ));
        assert_eq!(*events.borrow(), vec![Event::BusyTimeout]);
    }

    #[test]
    fn framebuffer_corners_follow_controller_stream_order() {
        let mut frame = E290FrameBuffer::new_white();
        frame
            .draw_iter([
                Pixel(Point::new(0, 0), BinaryColor::Off),
                Pixel(Point::new(0, 127), BinaryColor::Off),
                Pixel(Point::new(295, 0), BinaryColor::Off),
                Pixel(Point::new(295, 127), BinaryColor::Off),
            ])
            .unwrap();

        assert_eq!(frame.as_bytes()[0], 0x7f);
        assert_eq!(frame.as_bytes()[E290_BYTES_PER_COLUMN - 1], 0xfe);
        assert_eq!(
            frame.as_bytes()[(E290_FRAME_WIDTH - 1) * E290_BYTES_PER_COLUMN],
            0x7f
        );
        assert_eq!(frame.as_bytes()[E290_FRAME_BYTES - 1], 0xfe);
        assert!(
            frame.as_bytes()[1..E290_BYTES_PER_COLUMN - 1]
                .iter()
                .all(|byte| *byte == u8::MAX)
        );
    }

    #[test]
    fn framebuffer_clips_every_out_of_bounds_direction() {
        let mut frame = E290FrameBuffer::new_white();
        frame
            .draw_iter([
                Pixel(Point::new(-1, 0), BinaryColor::Off),
                Pixel(Point::new(0, -1), BinaryColor::Off),
                Pixel(Point::new(E290_FRAME_WIDTH as i32, 0), BinaryColor::Off),
                Pixel(Point::new(0, E290_FRAME_HEIGHT as i32), BinaryColor::Off),
            ])
            .unwrap();

        assert!(frame.as_bytes().iter().all(|byte| *byte == u8::MAX));
    }

    #[test]
    fn framebuffer_clear_matches_panel_polarity() {
        let mut frame = E290FrameBuffer::new_white();
        frame.clear(BinaryColor::Off);
        assert!(frame.as_bytes().iter().all(|byte| *byte == 0));

        DrawTarget::clear(&mut frame, BinaryColor::On).unwrap();
        assert!(frame.as_bytes().iter().all(|byte| *byte == u8::MAX));
    }

    #[derive(Clone, Copy, Debug)]
    struct SourceError;

    impl embedded_hal::digital::Error for SourceError {
        fn kind(&self) -> embedded_hal::digital::ErrorKind {
            embedded_hal::digital::ErrorKind::Other
        }
    }

    impl embedded_hal::spi::Error for SourceError {
        fn kind(&self) -> embedded_hal::spi::ErrorKind {
            embedded_hal::spi::ErrorKind::Other
        }
    }

    #[test]
    fn error_variants_retain_each_concrete_source_type() {
        fn assert_source(_: SourceError) {}

        match Ssd1680Error::<SourceError, Infallible, Infallible, Infallible>::Spi(SourceError) {
            Ssd1680Error::Spi(source) => assert_source(source),
            _ => panic!("wrong SPI error variant"),
        }
        match Ssd1680Error::<Infallible, SourceError, Infallible, Infallible>::DataCommand(
            SourceError,
        ) {
            Ssd1680Error::DataCommand(source) => assert_source(source),
            _ => panic!("wrong data/command error variant"),
        }
        match Ssd1680Error::<Infallible, Infallible, SourceError, Infallible>::Reset(SourceError) {
            Ssd1680Error::Reset(source) => assert_source(source),
            _ => panic!("wrong reset error variant"),
        }
        match Ssd1680Error::<Infallible, Infallible, Infallible, SourceError>::Busy(SourceError) {
            Ssd1680Error::Busy(source) => assert_source(source),
            _ => panic!("wrong BUSY error variant"),
        }
    }

    fn run<F: Future>(future: F) -> F::Output {
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[allow(dead_code)]
    fn _blocking_spi_is_not_the_driver_boundary<T: BlockingSpiDevice<u8>>() {}
}
