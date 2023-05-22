// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2022.

//! Chip trait setup.

use core::fmt::Write;
use cortexm4::{self, CortexM4, CortexMVariant};
use kernel::platform::chip::Chip;
use kernel::platform::chip::InterruptService;

use crate::dma;
use crate::nvic;

pub struct Stm32f4xx<'a, I: InterruptService + 'a> {
    mpu: cortexm4::mpu::MPU,
    userspace_kernel_boundary: cortexm4::syscall::SysCall,
    interrupt_service: &'a I,
}

pub struct Stm32f4xxDefaultPeripherals<'a> {
    pub adc1: crate::adc::Adc<'a>,
    pub dma1_streams: [crate::dma::Stream<'a, dma::Dma1<'a>>; 8],
    pub dma2_streams: [crate::dma::Stream<'a, dma::Dma2<'a>>; 8],
    pub exti: &'a crate::exti::Exti<'a>,
    pub i2c1: crate::i2c::I2C<'a>,
    pub spi3: crate::spi::Spi<'a>,
    pub tim2: crate::tim2::Tim2<'a>,
    pub usart1: crate::usart::Usart<'a, dma::Dma2<'a>>,
    pub usart2: crate::usart::Usart<'a, dma::Dma1<'a>>,
    pub usart3: crate::usart::Usart<'a, dma::Dma1<'a>>,
    pub gpio_ports: crate::gpio::GpioPorts<'a>,
    pub fsmc: crate::fsmc::Fsmc<'a>,
}

impl<'a> Stm32f4xxDefaultPeripherals<'a> {
    pub fn new(
        rcc: &'a crate::rcc::Rcc,
        exti: &'a crate::exti::Exti<'a>,
        dma1: &'a dma::Dma1<'a>,
        dma2: &'a dma::Dma2<'a>,
    ) -> Self {
        Self {
            adc1: crate::adc::Adc::new(rcc),
            dma1_streams: dma::new_dma1_stream(dma1),
            dma2_streams: dma::new_dma2_stream(dma2),
            exti,
            i2c1: crate::i2c::I2C::new(rcc),
            spi3: crate::spi::Spi::new(
                crate::spi::SPI3_BASE,
                crate::spi::SpiClock(crate::rcc::PeripheralClock::new(
                    crate::rcc::PeripheralClockType::APB1(crate::rcc::PCLK1::SPI3),
                    rcc,
                )),
                dma::Dma1Peripheral::SPI3_TX,
                dma::Dma1Peripheral::SPI3_RX,
            ),
            tim2: crate::tim2::Tim2::new(rcc),
            usart1: crate::usart::Usart::new_usart1(rcc),
            usart2: crate::usart::Usart::new_usart2(rcc),
            usart3: crate::usart::Usart::new_usart3(rcc),
            gpio_ports: crate::gpio::GpioPorts::new(rcc, exti),
            fsmc: crate::fsmc::Fsmc::new(
                [
                    Some(crate::fsmc::FSMC_BANK1),
                    None,
                    Some(crate::fsmc::FSMC_BANK3),
                    None,
                ],
                rcc,
            ),
        }
    }

    // Setup any circular dependencies and register deferred calls
    pub fn setup_circular_deps(&'static self) {
        self.gpio_ports.setup_circular_deps();

        // Note: Boards with a CAN bus present also need to register its
        // deferred call.
        kernel::deferred_call::DeferredCallClient::register(&self.usart1);
        kernel::deferred_call::DeferredCallClient::register(&self.usart2);
        kernel::deferred_call::DeferredCallClient::register(&self.usart3);
        kernel::deferred_call::DeferredCallClient::register(&self.fsmc);
    }
}

impl<'a> InterruptService for Stm32f4xxDefaultPeripherals<'a> {
    unsafe fn service_interrupt(&self, interrupt: u32) -> bool {
        match interrupt {
            nvic::DMA1_Stream1 => self.dma1_streams
                [dma::Dma1Peripheral::USART3_RX.get_stream_idx()]
            .handle_interrupt(),
            nvic::DMA1_Stream2 => {
                self.dma1_streams[dma::Dma1Peripheral::SPI3_RX.get_stream_idx()].handle_interrupt()
            }
            nvic::DMA1_Stream3 => self.dma1_streams
                [dma::Dma1Peripheral::USART3_TX.get_stream_idx()]
            .handle_interrupt(),
            nvic::DMA1_Stream5 => self.dma1_streams
                [dma::Dma1Peripheral::USART2_RX.get_stream_idx()]
            .handle_interrupt(),
            nvic::DMA1_Stream6 => self.dma1_streams
                [dma::Dma1Peripheral::USART2_TX.get_stream_idx()]
            .handle_interrupt(),
            nvic::DMA1_Stream7 => {
                self.dma1_streams[dma::Dma1Peripheral::SPI3_TX.get_stream_idx()].handle_interrupt()
            }

            nvic::DMA2_Stream5 => self.dma2_streams
                [dma::Dma2Peripheral::USART1_RX.get_stream_idx()]
            .handle_interrupt(),
            nvic::DMA2_Stream7 => self.dma2_streams
                [dma::Dma2Peripheral::USART1_TX.get_stream_idx()]
            .handle_interrupt(),

            nvic::USART1 => self.usart1.handle_interrupt(),
            nvic::USART2 => self.usart2.handle_interrupt(),
            nvic::USART3 => self.usart3.handle_interrupt(),

            nvic::ADC => self.adc1.handle_interrupt(),

            nvic::I2C1_EV => self.i2c1.handle_event(),
            nvic::I2C1_ER => self.i2c1.handle_error(),

            nvic::SPI3 => self.spi3.handle_interrupt(),

            nvic::EXTI0 => self.exti.handle_interrupt(),
            nvic::EXTI1 => self.exti.handle_interrupt(),
            nvic::EXTI2 => self.exti.handle_interrupt(),
            nvic::EXTI3 => self.exti.handle_interrupt(),
            nvic::EXTI4 => self.exti.handle_interrupt(),
            nvic::EXTI9_5 => self.exti.handle_interrupt(),
            nvic::EXTI15_10 => self.exti.handle_interrupt(),

            nvic::TIM2 => self.tim2.handle_interrupt(),

            _ => return false,
        }
        true
    }
}

impl<'a, I: InterruptService + 'a> Stm32f4xx<'a, I> {
    pub unsafe fn new(interrupt_service: &'a I) -> Self {
        Self {
            mpu: cortexm4::mpu::MPU::new(),
            userspace_kernel_boundary: cortexm4::syscall::SysCall::new(),
            interrupt_service,
        }
    }
}

impl<'a, I: InterruptService + 'a> Chip for Stm32f4xx<'a, I> {
    type MPU = cortexm4::mpu::MPU;
    type UserspaceKernelBoundary = cortexm4::syscall::SysCall;

    fn service_pending_interrupts(&self) {
        unsafe {
            loop {
                if let Some(interrupt) = cortexm4::nvic::next_pending() {
                    if !self.interrupt_service.service_interrupt(interrupt) {
                        panic!("unhandled interrupt {}", interrupt);
                    }

                    let n = cortexm4::nvic::Nvic::new(interrupt);
                    n.clear_pending();
                    n.enable();
                } else {
                    break;
                }
            }
        }
    }

    fn has_pending_interrupts(&self) -> bool {
        unsafe { cortexm4::nvic::has_pending() }
    }

    fn mpu(&self) -> &cortexm4::mpu::MPU {
        &self.mpu
    }

    fn userspace_kernel_boundary(&self) -> &cortexm4::syscall::SysCall {
        &self.userspace_kernel_boundary
    }

    fn sleep(&self) {
        unsafe {
            cortexm4::scb::unset_sleepdeep();
            cortexm4::support::wfi();
        }
    }

    unsafe fn atomic<F, R>(&self, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        cortexm4::support::atomic(f)
    }

    unsafe fn print_state(&self, write: &mut dyn Write) {
        CortexM4::print_cortexm_state(write);
    }
}
