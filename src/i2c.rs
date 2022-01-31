use core::ops::Deref;

use crate::pac::i2c1;
use crate::rcc::{Enable, Reset};

use crate::gpio::{Const, OpenDrain, PinA, SetAlternate};
#[cfg(feature = "i2c3")]
use crate::pac::I2C3;
use crate::pac::{I2C1, I2C2, RCC};

#[allow(unused)]
#[cfg(feature = "gpiof")]
use crate::gpio::gpiof;
#[allow(unused)]
use crate::gpio::{gpioa, gpiob, gpioc, gpioh};

use crate::rcc::Clocks;
use crate::time::{Hertz, U32Ext};

mod hal_02;
mod hal_1;

#[derive(Debug, Eq, PartialEq)]
pub enum DutyCycle {
    Ratio2to1,
    Ratio16to9,
}

#[derive(Debug, PartialEq)]
pub enum Mode {
    Standard {
        frequency: Hertz,
    },
    Fast {
        frequency: Hertz,
        duty_cycle: DutyCycle,
    },
}

impl Mode {
    pub fn standard<F: Into<Hertz>>(frequency: F) -> Self {
        Self::Standard {
            frequency: frequency.into(),
        }
    }

    pub fn fast<F: Into<Hertz>>(frequency: F, duty_cycle: DutyCycle) -> Self {
        Self::Fast {
            frequency: frequency.into(),
            duty_cycle,
        }
    }

    pub fn get_frequency(&self) -> Hertz {
        match *self {
            Self::Standard { frequency } => frequency,
            Self::Fast { frequency, .. } => frequency,
        }
    }
}

impl<F> From<F> for Mode
where
    F: Into<Hertz>,
{
    fn from(frequency: F) -> Self {
        let frequency: Hertz = frequency.into();
        if frequency <= 100_000.hz() {
            Self::Standard { frequency }
        } else {
            Self::Fast {
                frequency,
                duty_cycle: DutyCycle::Ratio2to1,
            }
        }
    }
}

/// I2C abstraction
pub struct I2c<I2C: Instance, PINS> {
    i2c: I2C,
    pins: PINS,
}

pub struct Scl;
impl crate::Sealed for Scl {}
pub struct Sda;
impl crate::Sealed for Sda {}

pub trait Pins<I2C> {
    fn set_alt_mode(&mut self);
    fn restore_mode(&mut self);
}

impl<I2C, SCL, SDA, const SCLA: u8, const SDAA: u8> Pins<I2C> for (SCL, SDA)
where
    SCL: PinA<Scl, I2C, A = Const<SCLA>> + SetAlternate<OpenDrain, SCLA>,
    SDA: PinA<Sda, I2C, A = Const<SDAA>> + SetAlternate<OpenDrain, SDAA>,
{
    fn set_alt_mode(&mut self) {
        self.0.set_alt_mode();
        self.1.set_alt_mode();
    }
    fn restore_mode(&mut self) {
        self.0.restore_mode();
        self.1.restore_mode();
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
#[non_exhaustive]
pub enum Error {
    OVERRUN,
    NACK,
    NACK_ADDR,
    NACK_DATA,
    TIMEOUT,
    // Note: The BUS error type is not currently returned, but is maintained for backwards
    // compatibility.
    BUS,
    CRC,
    ARBITRATION,
}

impl Error {
    pub(crate) fn nack_addr(self) -> Self {
        match self {
            Error::NACK => Error::NACK_ADDR,
            e => e,
        }
    }
    pub(crate) fn nack_data(self) -> Self {
        match self {
            Error::NACK => Error::NACK_DATA,
            e => e,
        }
    }
}

pub trait Instance: crate::Sealed + Deref<Target = i2c1::RegisterBlock> + Enable + Reset {}

impl Instance for I2C1 {}
impl Instance for I2C2 {}

#[cfg(feature = "i2c3")]
impl Instance for I2C3 {}

impl<I2C, PINS> I2c<I2C, PINS>
where
    I2C: Instance,
    PINS: Pins<I2C>,
{
    pub fn new<M: Into<Mode>>(i2c: I2C, mut pins: PINS, mode: M, clocks: &Clocks) -> Self {
        unsafe {
            // NOTE(unsafe) this reference will only be used for atomic writes with no side effects.
            let rcc = &(*RCC::ptr());

            // Enable and reset clock.
            I2C::enable(rcc);
            I2C::reset(rcc);
        }

        pins.set_alt_mode();

        let i2c = I2c { i2c, pins };
        i2c.i2c_init(mode, clocks.pclk1());
        i2c
    }

    pub fn release(mut self) -> (I2C, PINS) {
        self.pins.restore_mode();

        (self.i2c, self.pins)
    }
}

impl<I2C, PINS> I2c<I2C, PINS>
where
    I2C: Instance,
{
    fn i2c_init<M: Into<Mode>>(&self, mode: M, pclk: Hertz) {
        let mode = mode.into();
        // Make sure the I2C unit is disabled so we can configure it
        self.i2c.cr1.modify(|_, w| w.pe().clear_bit());

        // Calculate settings for I2C speed modes
        let clock = pclk.0;
        let clc_mhz = clock / 1_000_000;
        assert!((2..=50).contains(&clc_mhz));

        // Configure bus frequency into I2C peripheral
        self.i2c
            .cr2
            .write(|w| unsafe { w.freq().bits(clc_mhz as u8) });

        let trise = match mode {
            Mode::Standard { .. } => clc_mhz + 1,
            Mode::Fast { .. } => clc_mhz * 300 / 1000 + 1,
        };

        // Configure correct rise times
        self.i2c.trise.write(|w| w.trise().bits(trise as u8));

        match mode {
            // I2C clock control calculation
            Mode::Standard { frequency } => {
                let ccr = (clock / (frequency.0 * 2)).max(4);

                // Set clock to standard mode with appropriate parameters for selected speed
                self.i2c.ccr.write(|w| unsafe {
                    w.f_s()
                        .clear_bit()
                        .duty()
                        .clear_bit()
                        .ccr()
                        .bits(ccr as u16)
                });
            }
            Mode::Fast {
                frequency,
                duty_cycle,
            } => match duty_cycle {
                DutyCycle::Ratio2to1 => {
                    let ccr = (clock / (frequency.0 * 3)).max(1);

                    // Set clock to fast mode with appropriate parameters for selected speed (2:1 duty cycle)
                    self.i2c.ccr.write(|w| unsafe {
                        w.f_s().set_bit().duty().clear_bit().ccr().bits(ccr as u16)
                    });
                }
                DutyCycle::Ratio16to9 => {
                    let ccr = (clock / (frequency.0 * 25)).max(1);

                    // Set clock to fast mode with appropriate parameters for selected speed (16:9 duty cycle)
                    self.i2c.ccr.write(|w| unsafe {
                        w.f_s().set_bit().duty().set_bit().ccr().bits(ccr as u16)
                    });
                }
            },
        }

        // Enable the I2C processing
        self.i2c.cr1.modify(|_, w| w.pe().set_bit());
    }

    fn check_and_clear_error_flags(&self) -> Result<i2c1::sr1::R, Error> {
        // Note that flags should only be cleared once they have been registered. If flags are
        // cleared otherwise, there may be an inherent race condition and flags may be missed.
        let sr1 = self.i2c.sr1.read();

        if sr1.timeout().bit_is_set() {
            self.i2c.sr1.modify(|_, w| w.timeout().clear_bit());
            return Err(Error::TIMEOUT);
        }

        if sr1.pecerr().bit_is_set() {
            self.i2c.sr1.modify(|_, w| w.pecerr().clear_bit());
            return Err(Error::CRC);
        }

        if sr1.ovr().bit_is_set() {
            self.i2c.sr1.modify(|_, w| w.ovr().clear_bit());
            return Err(Error::OVERRUN);
        }

        if sr1.af().bit_is_set() {
            self.i2c.sr1.modify(|_, w| w.af().clear_bit());
            return Err(Error::NACK);
        }

        if sr1.arlo().bit_is_set() {
            self.i2c.sr1.modify(|_, w| w.arlo().clear_bit());
            return Err(Error::ARBITRATION);
        }

        // The errata indicates that BERR may be incorrectly detected. It recommends ignoring and
        // clearing the BERR bit instead.
        if sr1.berr().bit_is_set() {
            self.i2c.sr1.modify(|_, w| w.berr().clear_bit());
        }

        Ok(sr1)
    }
}

trait I2cCommon {
    fn write_bytes(&mut self, addr: u8, bytes: impl Iterator<Item = u8>) -> Result<(), Error>;

    fn send_byte(&self, byte: u8) -> Result<(), Error>;

    fn recv_byte(&self) -> Result<u8, Error>;
}

impl<I2C, PINS> I2cCommon for I2c<I2C, PINS>
where
    I2C: Instance,
{
    fn write_bytes(&mut self, addr: u8, bytes: impl Iterator<Item = u8>) -> Result<(), Error> {
        // Send a START condition
        self.i2c.cr1.modify(|_, w| w.start().set_bit());

        // Wait until START condition was generated
        while self.check_and_clear_error_flags()?.sb().bit_is_clear() {}

        // Also wait until signalled we're master and everything is waiting for us
        loop {
            self.check_and_clear_error_flags()?;

            let sr2 = self.i2c.sr2.read();
            if !(sr2.msl().bit_is_clear() && sr2.busy().bit_is_clear()) {
                break;
            }
        }

        // Set up current address, we're trying to talk to
        self.i2c
            .dr
            .write(|w| unsafe { w.bits(u32::from(addr) << 1) });

        // Wait until address was sent
        loop {
            // Check for any I2C errors. If a NACK occurs, the ADDR bit will never be set.
            let sr1 = self
                .check_and_clear_error_flags()
                .map_err(Error::nack_addr)?;

            // Wait for the address to be acknowledged
            if sr1.addr().bit_is_set() {
                break;
            }
        }

        // Clear condition by reading SR2
        self.i2c.sr2.read();

        // Send bytes
        for c in bytes {
            self.send_byte(c)?;
        }

        // Fallthrough is success
        Ok(())
    }

    fn send_byte(&self, byte: u8) -> Result<(), Error> {
        // Wait until we're ready for sending
        // Check for any I2C errors. If a NACK occurs, the ADDR bit will never be set.
        while self
            .check_and_clear_error_flags()
            .map_err(Error::nack_addr)?
            .tx_e()
            .bit_is_clear()
        {}

        // Push out a byte of data
        self.i2c.dr.write(|w| unsafe { w.bits(u32::from(byte)) });

        // Wait until byte is transferred
        // Check for any potential error conditions.
        while self
            .check_and_clear_error_flags()
            .map_err(Error::nack_data)?
            .btf()
            .bit_is_clear()
        {}

        Ok(())
    }

    fn recv_byte(&self) -> Result<u8, Error> {
        loop {
            // Check for any potential error conditions.
            self.check_and_clear_error_flags()
                .map_err(Error::nack_data)?;

            if self.i2c.sr1.read().rx_ne().bit_is_set() {
                break;
            }
        }

        let value = self.i2c.dr.read().bits() as u8;
        Ok(value)
    }
}
