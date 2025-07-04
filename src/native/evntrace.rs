//! Safe wrappers for the native ETW API
//!
//! This module makes sure the calls are safe memory-wise, but does not attempt to ensure they are called in the right order.<br/>
//! Thus, you should prefer using `UserTrace`s, `KernelTrace`s and `TraceBuilder`s, that will ensure these API are correctly used.
use std::collections::HashSet;
use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::Mutex;

use once_cell::sync::Lazy;

use widestring::U16CStr;
use windows::core::GUID;
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
use windows::Win32::Foundation::ERROR_CTX_CLOSE_PENDING;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::Diagnostics::Etw;
use windows::Win32::System::Diagnostics::Etw::EVENT_CONTROL_CODE_ENABLE_PROVIDER;
use windows::Win32::System::Diagnostics::Etw::TRACE_QUERY_INFO_CLASS;

use super::etw_types::*;
use crate::native::etw_types::event_record::EventRecord;
use crate::provider::event_filter::EventFilterDescriptor;
use crate::provider::Provider;
use crate::trace::callback_data::CallbackData;
use crate::trace::{RealTimeTraceTrait, TraceProperties};

pub type TraceHandle = Etw::PROCESSTRACE_HANDLE;
pub type ControlHandle = Etw::CONTROLTRACE_HANDLE;

/// Evntrace native module errors
#[derive(Debug)]
pub enum EvntraceNativeError {
    /// Represents an Invalid Handle Error
    InvalidHandle,
    /// Represents an ERROR_ALREADY_EXISTS
    AlreadyExist,
    /// Represents an standard IO Error
    IoError(std::io::Error),
}

pub(crate) type EvntraceNativeResult<T> = Result<T, EvntraceNativeError>;

/// When a trace is closing, it is possible that every past events have not been processed yet.
/// These events will still be fed to the callback, **after** the trace has been closed
/// (see `ERROR_CTX_CLOSE_PENDING` in https://learn.microsoft.com/en-us/windows/win32/api/evntrace/nf-evntrace-closetrace#remarks)
/// Also, there is no way to tell which callback invocation is the last one.
///
/// But, we would like to free memory used by the callbacks when we're done!
/// Since that is not possible, let's discard every callback run after we've called `CloseTrace`.
/// That's the purpose of this set.
///
/// TODO: it _might_ be possible to know whether we've processed the last buffered event, as
///       ControlTraceW(EVENT_TRACE_CONTROL_QUERY) _might_ tell us if the buffers are empty or not.
///       In case the trace is in ERROR_CTX_CLOSE_PENDING state, we could call this after every
///       callback so that we know when to actually free memory used by the (now useless) callback.
///       Maybe also setting the BufferCallback in EVENT_TRACE_LOGFILEW may help us.
///       That's <https://github.com/n4r1b/ferrisetw/issues/62>
static UNIQUE_VALID_CONTEXTS: UniqueValidContexts = UniqueValidContexts::new();
struct UniqueValidContexts(Lazy<Mutex<HashSet<u64>>>);
enum ContextError {
    AlreadyExist,
}

impl UniqueValidContexts {
    pub const fn new() -> Self {
        Self(Lazy::new(|| Mutex::new(HashSet::new())))
    }
    /// Insert if it did not exist previously
    fn insert(&self, ctx_ptr: *const c_void) -> Result<(), ContextError> {
        match self.0.lock().unwrap().insert(ctx_ptr as u64) {
            true => Ok(()),
            false => Err(ContextError::AlreadyExist),
        }
    }

    fn remove(&self, ctx_ptr: *const c_void) {
        self.0.lock().unwrap().remove(&(ctx_ptr as u64));
    }

    pub fn is_valid(&self, ctx_ptr: *const c_void) -> bool {
        self.0.lock().unwrap().contains(&(ctx_ptr as u64))
    }
}

/// This will be called by the ETW framework whenever an ETW event is available
extern "system" fn trace_callback_thunk(p_record: *mut Etw::EVENT_RECORD) {
    match std::panic::catch_unwind(AssertUnwindSafe(|| {
        let record_from_ptr = unsafe {
            // Safety: lifetime is valid at least until the end of the callback. A correct lifetime will be attached when we pass the reference to the child function
            EventRecord::from_ptr(p_record)
        };

        if let Some(event_record) = record_from_ptr {
            let p_user_context = event_record.user_context();
            if !UNIQUE_VALID_CONTEXTS.is_valid(p_user_context) {
                return;
            }
            let p_callback_data = p_user_context.cast::<Arc<CallbackData>>();
            let callback_data = unsafe {
                // Safety:
                //  * the API of this create guarantees this points to a `CallbackData` already allocated and created
                //  * we've just checked using UNIQUE_VALID_CONTEXTS that this `CallbackData` has not been dropped
                //  * the API of this crate guarantees this `CallbackData` is not mutated from another thread during the trace:
                //      * we're the only one to change CallbackData::events_handled (and that's an atomic, so it's fine)
                //      * the list of Providers is a constant (may change in the future with #54)
                //      * the schema_locator only has interior mutability
                p_callback_data.as_ref()
            };
            if let Some(callback_data) = callback_data {
                // The UserContext is owned by the `Trace` object. When it is dropped, so will the UserContext.
                // We clone it now, so that the original Arc can be safely dropped at all times, but the callback data (including the closure captured context) will still be alive until the callback ends.
                let cloned_arc = Arc::clone(callback_data);
                cloned_arc.on_event(event_record);
            }
        }
    })) {
        Ok(_) => {}
        Err(e) => {
            log::error!("UNIMPLEMENTED PANIC: {e:?}");
            std::process::exit(1);
        }
    }
}

fn filter_invalid_trace_handles(h: TraceHandle) -> Option<TraceHandle> {
    // See https://learn.microsoft.com/en-us/windows/win32/api/evntrace/nf-evntrace-opentracew#return-value
    // We're conservative and we always filter out u32::MAX, although it could be valid on 64-bit setups.
    // But it turns out runtime detection of the current OS bitness is not that easy. Plus, it is not clear whether this depends on how the architecture the binary is compiled for, or the actual OS architecture.
    if h.Value == u64::MAX || h.Value == u32::MAX as u64 {
        None
    } else {
        Some(h)
    }
}

fn filter_invalid_control_handle(h: ControlHandle) -> Option<ControlHandle> {
    // The control handle is 0 if the handle is not valid.
    // (https://learn.microsoft.com/en-us/windows/win32/api/evntrace/nf-evntrace-starttracew)
    if h.Value == 0 {
        None
    } else {
        Some(h)
    }
}

/// Create a new session.
///
/// This builds an `EventTraceProperties`, calls `StartTraceW` and returns the built `EventTraceProperties` as well as the trace ControlHandle
pub(crate) fn start_trace<T>(
    trace_name: &U16CStr,
    etl_dump_file: Option<(&U16CStr, DumpFileLoggingMode, Option<u32>)>,
    trace_properties: &TraceProperties,
    enable_flags: Etw::EVENT_TRACE_FLAG,
) -> EvntraceNativeResult<(EventTraceProperties, ControlHandle)>
where
    T: RealTimeTraceTrait,
{
    let mut properties =
        EventTraceProperties::new::<T>(trace_name, etl_dump_file, trace_properties, enable_flags);

    let mut control_handle = ControlHandle::default();
    let status = unsafe {
        // Safety:
        //  * first argument points to a valid and allocated address (this is an output and will be modified)
        //  * second argument is a valid, null terminated widestring (note that it will be copied to the EventTraceProperties...from where it already comes. This will probably be overwritten by Windows, but heck.)
        //  * third argument is a valid, allocated EVENT_TRACE_PROPERTIES (and will be mutated)
        //  * Note: the string (that will be overwritten to itself) ends with a null widechar before the end of its buffer (see EventTraceProperties::new())
        Etw::StartTraceW(
            &mut control_handle,
            PCWSTR::from_raw(properties.trace_name_array().as_ptr()),
            properties.as_mut_ptr(),
        )
    }
    .ok();

    if let Err(status) = status {
        let code = status.code();

        if code == ERROR_ALREADY_EXISTS.to_hresult() {
            return Err(EvntraceNativeError::AlreadyExist);
        } else if code != ERROR_SUCCESS.to_hresult() {
            return Err(EvntraceNativeError::IoError(
                std::io::Error::from_raw_os_error(code.0),
            ));
        }
    }

    match filter_invalid_control_handle(control_handle) {
        None => Err(EvntraceNativeError::InvalidHandle),
        Some(handle) => Ok((properties, handle)),
    }
}

/// Connect to an existing session.
///
/// This queries an existing session by name and returns the EventTraceProperties and a special ControlHandle 
/// that represents name-based control for the existing session.
pub(crate) fn open_existing_trace<T>(
    trace_name: &U16CStr,
    trace_properties: &TraceProperties,
) -> EvntraceNativeResult<(EventTraceProperties, ControlHandle)>
where
    T: RealTimeTraceTrait,
{
    // Query the existing session to get its current properties
    let mut properties = EventTraceProperties::new::<T>(
        trace_name,
        None, // When opening existing session, we don't specify ETL file
        trace_properties,
        Etw::EVENT_TRACE_FLAG::default(),
    );

    // Use ControlTraceW with EVENT_TRACE_CONTROL_QUERY to verify session exists and get its info
    let status = unsafe {
        Etw::ControlTraceW(
            ControlHandle::default(), // Handle 0 means we're using session name
            PCWSTR::from_raw(trace_name.as_ptr()),
            properties.as_mut_ptr(),
            Etw::EVENT_TRACE_CONTROL_QUERY,
        )
    }
    .ok();

    if let Err(status) = status {
        let code = status.code();
        return Err(EvntraceNativeError::IoError(
            std::io::Error::from_raw_os_error(code.0),
        ));
    }

    // Extract the actual control handle from the queried properties
    // After a successful query, the Wnode.HistoricalContext field contains the session handle
    let control_handle = unsafe {
        let event_trace_props_ptr = &mut properties as *mut EventTraceProperties as *mut Etw::EVENT_TRACE_PROPERTIES;
        let historical_context = (*event_trace_props_ptr).Wnode.Anonymous1.HistoricalContext;
        ControlHandle { Value: historical_context }
    };

    Ok((properties, control_handle))
}

/// Subscribe to a started trace
///
/// Microsoft calls this "opening" the trace (and this calls `OpenTraceW`)
#[allow(clippy::borrowed_box)] // Being Boxed is really important, let's keep the Box<...> in the function signature to make the intent clearer
pub(crate) fn open_trace(
    subscription_source: SubscriptionSource,
    callback_data: &Box<Arc<CallbackData>>,
) -> EvntraceNativeResult<TraceHandle> {
    let mut log_file =
        EventTraceLogfile::create(callback_data, subscription_source, trace_callback_thunk);

    if let Err(ContextError::AlreadyExist) = UNIQUE_VALID_CONTEXTS.insert(log_file.context_ptr()) {
        // That's probably possible to get multiple handles to the same trace, by opening them multiple times.
        // But that's left as a future TODO. Making things right and safe is difficult enough with a single opening of the trace already.
        return Err(EvntraceNativeError::AlreadyExist);
    }

    let trace_handle = unsafe {
        // This function modifies the data pointed to by log_file.
        // This is fine because there is currently no other ref `self` (the current function takes a `&mut self`, and `self` is not used anywhere else in the current function)
        //
        // > On success, OpenTrace will update the structure with information from the opened file or session.
        // https://learn.microsoft.com/en-us/windows/win32/api/evntrace/nf-evntrace-opentracea
        Etw::OpenTraceW(log_file.as_mut_ptr())
    };

    if filter_invalid_trace_handles(trace_handle).is_none() {
        Err(EvntraceNativeError::IoError(std::io::Error::last_os_error()))
    } else {
        Ok(trace_handle)
    }
}

/// Attach a provider to a trace
pub(crate) fn enable_provider(
    control_handle: ControlHandle,
    provider: &Provider,
) -> EvntraceNativeResult<()> {
    match filter_invalid_control_handle(control_handle) {
        None => Err(EvntraceNativeError::InvalidHandle),
        Some(handle) => {
            let owned_event_filter_descriptors: Vec<EventFilterDescriptor> = provider
                .filters()
                .iter()
                .filter_map(|filter| filter.to_event_filter_descriptor().ok()) // Silently ignoring invalid filters (basically, empty ones)
                .collect();

            let parameters = EnableTraceParameters::create(
                provider.guid(),
                provider.trace_flags(),
                &owned_event_filter_descriptors,
            );

            let res = unsafe {
                Etw::EnableTraceEx2(
                    handle,
                    &provider.guid() as *const GUID,
                    EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                    provider.level(),
                    provider.any(),
                    provider.all(),
                    0,
                    Some(parameters.as_ptr()),
                )
            }
            .ok();

            res.map_err(|err| {
                EvntraceNativeError::IoError(std::io::Error::from_raw_os_error(err.code().0))
            })
        }
    }
}

/// Start processing a trace (this call is blocking until the trace is stopped)
///
/// You probably want to spawn a thread that will block on this call.
pub(crate) fn process_trace(trace_handle: TraceHandle) -> EvntraceNativeResult<()> {
    if filter_invalid_trace_handles(trace_handle).is_none() {
        Err(EvntraceNativeError::InvalidHandle)
    } else {
        let result = unsafe {
            // We want to start processing events as soon as January 1601.
            // * for ETL file traces, this is fine, this means "process everything from the file"
            // * for real-time traces, this means we might process a few events already waiting in the buffers when the processing is starting. This is fine, I suppose.
            let mut start = FILETIME::default();
            Etw::ProcessTrace(&[trace_handle], Some(&mut start as *mut FILETIME), None)
        }
        .ok();

        result.map_err(|err| {
            EvntraceNativeError::IoError(std::io::Error::from_raw_os_error(err.code().0))
        })
    }
}

/// Call `ControlTraceW` on the trace
///
/// # Notes
///
/// In case you want to stop the trace, you probably want to drop the instance rather than calling `control(EVENT_TRACE_CONTROL_STOP)` yourself,
/// because stop the trace makes the trace handle invalid.
/// A stopped trace could theoretically(?) be re-used, but the trace handle should be re-created, so `open` should be called again.
pub(crate) fn control_trace(
    properties: &mut EventTraceProperties,
    control_handle: ControlHandle,
    control_code: Etw::EVENT_TRACE_CONTROL,
) -> EvntraceNativeResult<()> {
    // Use normal handle-based control for all sessions (including existing ones)
    match filter_invalid_control_handle(control_handle) {
        None => Err(EvntraceNativeError::InvalidHandle),
        Some(handle) => {
            let result = unsafe {
                // Safety:
                //  * the trace handle is valid (by construction)
                //  * depending on the control code, the `Properties` can be mutated. This is fine because properties is declared as `&mut` in this function, which means no other Rust function has a reference to it, and the mutation can only happen in the call to `ControlTraceW`, which returns immediately.
                Etw::ControlTraceW(
                    handle,
                    PCWSTR::null(),
                    properties.as_mut_ptr(),
                    control_code,
                )
            }
            .ok();

            result.map_err(|err| {
                EvntraceNativeError::IoError(std::io::Error::from_raw_os_error(err.code().0))
            })
        }
    }
}

/// Similar to [`control_trace`], but using a trace name instead of a handle
pub(crate) fn control_trace_by_name(
    properties: &mut EventTraceProperties,
    trace_name: &U16CStr,
    control_code: Etw::EVENT_TRACE_CONTROL,
) -> EvntraceNativeResult<()> {
    let result = unsafe {
        // Safety:
        //  * depending on the control code, the `Properties` can be mutated. This is fine because properties is declared as `&mut` in this function, which means no other Rust function has a reference to it, and the mutation can only happen in the call to `ControlTraceW`, which returns immediately.
        Etw::ControlTraceW(
            Etw::CONTROLTRACE_HANDLE { Value: 0 },
            PCWSTR::from_raw(trace_name.as_ptr()),
            properties.as_mut_ptr(),
            control_code,
        )
    }
    .ok();

    result.map_err(|err| {
        EvntraceNativeError::IoError(std::io::Error::from_raw_os_error(err.code().0))
    })
}

/// Close the trace
///
/// It is suggested to stop the trace immediately after `close`ing it (that's what it done in the `impl Drop`), because I'm not sure how sensible it is to call other methods (apart from `stop`) afterwards
///
/// In case ETW reports there are still events in the queue that are still to trigger callbacks, this returns Ok(true).<br/>
/// If no further event callback will be invoked, this returns Ok(false)<br/>
/// On error, this returns an `Err`
#[allow(clippy::borrowed_box)] // Being Boxed is really important, let's keep the Box<...> in the function signature to make the intent clearer
pub(crate) fn close_trace(
    trace_handle: TraceHandle,
    callback_data: &Box<Arc<CallbackData>>,
) -> EvntraceNativeResult<bool> {
    match filter_invalid_trace_handles(trace_handle) {
        None => Err(EvntraceNativeError::InvalidHandle),
        Some(handle) => {
            // By contruction, only one Provider used this context in its callback. It is safe to remove it, it won't be used by anyone else.
            UNIQUE_VALID_CONTEXTS
                .remove(callback_data.as_ref() as *const Arc<CallbackData> as *const c_void);

            let status = unsafe { Etw::CloseTrace(handle) }.ok();

            match status {
                Ok(()) => Ok(false),
                Err(err) if err.code() == ERROR_CTX_CLOSE_PENDING.to_hresult() => Ok(true),
                Err(err) => Err(EvntraceNativeError::IoError(
                    std::io::Error::from_raw_os_error(err.code().0),
                )),
            }
        }
    }
}

/// Queries the system for system-wide ETW information (that does not require an active session).
pub(crate) fn query_info(class: TraceInformation, buf: &mut [u8]) -> EvntraceNativeResult<()> {
    let result = unsafe {
        Etw::TraceQueryInformation(
            Etw::CONTROLTRACE_HANDLE { Value: 0 },
            TRACE_QUERY_INFO_CLASS(class as i32),
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            None,
        )
    }
    .ok();

    result.map_err(|err| {
        EvntraceNativeError::IoError(std::io::Error::from_raw_os_error(err.code().0))
    })
}
