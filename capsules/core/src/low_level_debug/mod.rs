// Licensed under the Apache License, Version 2.0 or the MIT License.
// SPDX-License-Identifier: Apache-2.0 OR MIT
// Copyright Tock Contributors 2022.

//! Provides low-level debugging functionality to userspace. The system call
//! interface is documented in doc/syscalls/00008_low_level_debug.md.

mod fmt;

use core::cell::Cell;
use core::usize;

use kernel::grant::{AllowRoCount, AllowRwCount, Grant, UpcallCount};
use kernel::hil::uart::{Transmit, TransmitClient};
use kernel::syscall::CommandReturn;
use kernel::utilities::packet_buffer::{self, PacketBufferDyn, PacketBufferMut, PacketSliceMut};
use kernel::{ErrorCode, ProcessId};

// LowLevelDebug requires a &mut [u8] buffer of length at least BUF_LEN.
pub use fmt::BUF_LEN;

pub const DRIVER_NUM: usize = crate::driver::NUM::LowLevelDebug as usize;

pub struct LowLevelDebug<
    'u,
    U: Transmit<'u, HEAD_TRANSMIT, TAIL_TRANSMIT>,
    const HEAD: usize,
    const TAIL: usize,
    const HEAD_TRANSMIT: usize,
    const TAIL_TRANSMIT: usize,
> {
    buffer: Cell<Option<PacketBufferMut<HEAD, TAIL>>>,
    grant: Grant<AppData, UpcallCount<0>, AllowRoCount<0>, AllowRwCount<0>>,
    // grant_failed is set to true when LowLevelDebug fails to allocate an app's
    // grant region. When it has a chance, LowLevelDebug will print a message
    // indicating a grant initialization has failed, then set this back to
    // false. Although LowLevelDebug cannot print an application ID without
    // using grant storage, it will at least output an error indicating some
    // application's message was dropped.
    grant_failed: Cell<bool>,
    uart: &'u U,
}

impl<
        'u,
        U: Transmit<'u, HEAD_TRANSMIT, TAIL_TRANSMIT>,
        const HEAD: usize,
        const TAIL: usize,
        const HEAD_TRANSMIT: usize,
        const TAIL_TRANSMIT: usize,
    > LowLevelDebug<'u, U, HEAD, TAIL, HEAD_TRANSMIT, TAIL_TRANSMIT>
{
    pub fn new(
        buffer: PacketBufferMut<HEAD, TAIL>,
        uart: &'u U,
        grant: Grant<AppData, UpcallCount<0>, AllowRoCount<0>, AllowRwCount<0>>,
    ) -> LowLevelDebug<'u, U, HEAD, TAIL, HEAD_TRANSMIT, TAIL_TRANSMIT> {
        LowLevelDebug {
            buffer: Cell::new(Some(buffer)),
            grant,
            grant_failed: Cell::new(false),
            uart,
        }
    }
}

impl<
        'u,
        U: Transmit<'u, HEAD_TRANSMIT, TAIL_TRANSMIT>,
        const HEAD: usize,
        const TAIL: usize,
        const HEAD_TRANSMIT: usize,
        const TAIL_TRANSMIT: usize,
    > kernel::syscall::SyscallDriver
    for LowLevelDebug<'u, U, HEAD, TAIL, HEAD_TRANSMIT, TAIL_TRANSMIT>
{
    fn command(
        &self,
        minor_num: usize,
        r2: usize,
        r3: usize,
        caller_id: ProcessId,
    ) -> CommandReturn {
        match minor_num {
            0 => return CommandReturn::success(),
            1 => self.push_entry(DebugEntry::AlertCode(r2), caller_id),
            2 => self.push_entry(DebugEntry::Print1(r2), caller_id),
            3 => self.push_entry(DebugEntry::Print2(r2, r3), caller_id),
            _ => return CommandReturn::failure(ErrorCode::NOSUPPORT),
        }
        CommandReturn::success()
    }

    fn allocate_grant(&self, processid: ProcessId) -> Result<(), kernel::process::Error> {
        self.grant.enter(processid, |_, _| {})
    }
}

impl<
        'u,
        U: Transmit<'u, HEAD_TRANSMIT, TAIL_TRANSMIT>,
        const HEAD: usize,
        const TAIL: usize,
        const HEAD_TRANSMIT: usize,
        const TAIL_TRANSMIT: usize,
    > TransmitClient<HEAD_TRANSMIT, TAIL_TRANSMIT>
    for LowLevelDebug<'u, U, HEAD, TAIL, HEAD_TRANSMIT, TAIL_TRANSMIT>
{
    fn transmitted_buffer(
        &self,
        mut tx_buffer: PacketBufferMut<HEAD_TRANSMIT, TAIL_TRANSMIT>,
        _tx_len: usize,
        _rval: Result<(), ErrorCode>,
    ) {
        // Identify and transmit the next queued entry. If there are no queued
        // entries remaining, store buffer.

        // Prioritize printing the "grant init failed" message over per-app
        // debug entries.
        if self.grant_failed.take() {
            const MESSAGE: &[u8] = b"LowLevelDebug: grant init failed\n";

            // tx_buffer[..MESSAGE.len()].copy_from_slice(MESSAGE);
            // TODO: device whether we should do something in case of failure or not
            let _ = tx_buffer.copy_from_slice_or_err(MESSAGE);

            let _ = self.uart.transmit_buffer(tx_buffer, MESSAGE.len()).map_err(
                |(_, returned_buffer)| {
                    self.buffer.set(Some(returned_buffer.reset().unwrap()))
                    // self.buffer.set(Some(buffer));
                },
            );
            return;
        }

        for process_grant in self.grant.iter() {
            let processid = process_grant.processid();
            let (app_num, first_entry) = process_grant.enter(|owned_app_data, _| {
                owned_app_data.queue.rotate_left(1);
                (processid.id(), owned_app_data.queue[QUEUE_SIZE - 1].take())
            });
            let to_print = match first_entry {
                None => continue,
                Some(to_print) => to_print,
            };

            // let headroom = tx_buffer.headroom();
            // let tailroom = tx_buffer.tailroom();
            // let capacity = tx_buffer.capacity();
            // let slice = tx_buffer
            //     .downcast::<PacketSliceMut>()
            //     .unwrap()
            //     .data_slice_mut();
            self.transmit_entry(tx_buffer, app_num, to_print);
            return;
        }

        self.buffer.set(Some(tx_buffer.reset().unwrap()))
        // self.buffer.set(Some(tx_buffer));
    }
}

// -----------------------------------------------------------------------------
// Implementation details below
// -----------------------------------------------------------------------------

impl<
        'u,
        U: Transmit<'u, HEAD_TRANSMIT, TAIL_TRANSMIT>,
        const HEAD: usize,
        const TAIL: usize,
        const HEAD_TRANSMIT: usize,
        const TAIL_TRANSMIT: usize,
    > LowLevelDebug<'u, U, HEAD, TAIL, HEAD_TRANSMIT, TAIL_TRANSMIT>
{
    // If the UART is not busy (the buffer is available), transmits the entry.
    // Otherwise, adds it to the app's queue.
    fn push_entry(&self, entry: DebugEntry, processid: ProcessId) {
        use DebugEntry::Dropped;

        if let Some(buffer) = self.buffer.take() {
            // let headroom = buffer.headroom();
            // let tailroom = buffer.tailroom();
            // let slice = buffer
            //     .downcast::<PacketSliceMut>()
            //     .unwrap()
            //     .data_slice_mut();
            let new_head_buf = buffer.reduce_headroom().reduce_tailroom();
            self.transmit_entry(new_head_buf, processid.id(), entry);
            return;
        }

        let result = self.grant.enter(processid, |borrow, _| {
            for queue_entry in &mut borrow.queue {
                if queue_entry.is_none() {
                    *queue_entry = Some(entry);
                    return;
                }
            }
            // The queue is full, so indicate some entries were dropped. If
            // there is not a drop entry, replace the last entry with a drop
            // counter. A new drop counter is initialized to two, as the
            // overwrite drops an entry plus we're dropping this entry.
            borrow.queue[QUEUE_SIZE - 1] = match borrow.queue[QUEUE_SIZE - 1] {
                Some(Dropped(count)) => Some(Dropped(count + 1)),
                _ => Some(Dropped(2)),
            };
        });

        // If we were unable to enter the grant region, schedule a diagnostic
        // message. This gives the user a chance of figuring out what happened
        // when LowLevelDebug fails.
        if result.is_err() {
            self.grant_failed.set(true);
        }
    }

    // Immediately prints the provided entry to the UART.
    fn transmit_entry(
        &self,
        mut buffer: PacketBufferMut<HEAD_TRANSMIT, TAIL_TRANSMIT>,
        app_num: usize,
        entry: DebugEntry,
    ) {
        let msg_len = fmt::format_entry(app_num, entry, &mut buffer.payload_mut());
        // The uart's error message is ignored because we cannot do anything if
        // it fails anyway.
        let _ = self
            .uart
            .transmit_buffer(buffer, msg_len)
            .map_err(|(_, returned_buffer)| {
                // let buf = returned_buffer
                //     .downcast::<PacketSliceMut>()
                //     .unwrap()
                //     .into_inner();

                let pb = returned_buffer
                    .reclaim_headroom()
                    .unwrap()
                    .reclaim_tailroom()
                    .unwrap();

                // let new_head_buf = returned_buffer.restore_headroom().unwrap();
                // let new_buf = new_head_buf.restore_tailroom().unwrap();

                // AMALIA: asta sigur nu e ok. Aveam nevoie de un pbmut cu new head si in loc sa ii fac reclaim am facut altu :))))
                // let pb = PacketBufferMut::new(PacketSliceMut::new(buf).unwrap()).unwrap();

                self.buffer.set(Some(pb));
            });
    }
}

// Length of the debug queue for each app. Each queue entry takes 3 words (tag
// and 2 usizes to print). The queue will be allocated in an app's grant region
// when that app first uses the debug driver.
const QUEUE_SIZE: usize = 4;

#[derive(Default)]
pub struct AppData {
    queue: [Option<DebugEntry>; QUEUE_SIZE],
}

#[derive(Clone, Copy)]
pub(crate) enum DebugEntry {
    Dropped(usize),       // Some debug messages were dropped
    AlertCode(usize),     // Display a predefined alert code
    Print1(usize),        // Print a single number
    Print2(usize, usize), // Print two numbers
}
