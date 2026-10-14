//! ETW Event Schema and handler
//!
//! This module contains the means needed to interact with the Schema of an ETW event
use crate::native::etw_types::DecodingSource;
use crate::native::tdh::TraceEventInfo;
use crate::native::tdh_types::{Property, PropertyError};
use once_cell::sync::OnceCell;

/// A schema suitable for parsing a given kind of event.
///
/// It is usually retrieved from [`crate::schema_locator::SchemaLocator::event_schema`].
///
/// This structure is basically a wrapper over a [TraceEventInfo](https://docs.microsoft.com/en-us/windows/win32/api/tdh/ns-tdh-trace_event_info),
/// with a few info parsed (and cached) out of it
pub struct Schema {
    te_info: TraceEventInfo,
    cached_properties: OnceCell<Result<Vec<Property>, PropertyError>>,
    /// [`Self::static_offsets`] for events with 4-byte pointers.
    static_offsets_32: OnceCell<Vec<Option<usize>>>,
    /// [`Self::static_offsets`] for events with 8-byte pointers.
    static_offsets_64: OnceCell<Vec<Option<usize>>>,
}

impl Schema {
    pub(crate) fn new(te_info: TraceEventInfo) -> Self {
        Schema {
            te_info,
            cached_properties: OnceCell::new(),
            static_offsets_32: OnceCell::new(),
            static_offsets_64: OnceCell::new(),
        }
    }

    /// The byte offset of each property in the user buffer of an event with
    /// `pointer_size`-byte pointers, for the properties whose offset does not
    /// depend on the event's content: every property before them has a size
    /// the schema fixes. `None` for a property that follows one of variable
    /// size (a null-terminated string, a length or count read from another
    /// property). Computed once per pointer size.
    pub(crate) fn static_offsets(&self, pointer_size: usize) -> &[Option<usize>] {
        let cell = if pointer_size == 4 {
            &self.static_offsets_32
        } else {
            &self.static_offsets_64
        };
        cell.get_or_init(|| {
            let properties = self.properties();
            let mut offsets = Vec::with_capacity(properties.len());
            let mut next = Some(0usize);
            for property in properties {
                offsets.push(next);
                next = match (next, property.static_size(pointer_size)) {
                    (Some(offset), Some(size)) => offset.checked_add(size),
                    _ => None,
                };
            }
            offsets
        })
    }

    /// Use the `decoding_source` function to obtain the [DecodingSource] from the `TRACE_EVENT_INFO`
    ///
    /// This getter returns the DecodingSource from the event, this value identifies the source used
    /// parse the event data
    ///
    /// # Example
    /// ```
    /// # use ferrisetw::EventRecord;
    /// # use ferrisetw::schema_locator::SchemaLocator;

    /// let my_callback = |record: &EventRecord, schema_locator: &SchemaLocator| {
    ///     let schema = schema_locator.event_schema(record).unwrap();
    ///     let decoding_source = schema.decoding_source();
    /// };
    /// ```
    pub fn decoding_source(&self) -> DecodingSource {
        self.te_info.decoding_source()
    }

    /// Use the `provider_name` function to obtain the Provider name from the `TRACE_EVENT_INFO`
    ///
    /// # Example
    /// ```
    /// # use ferrisetw::EventRecord;
    /// # use ferrisetw::schema_locator::SchemaLocator;
    /// let my_callback = |record: &EventRecord, schema_locator: &SchemaLocator| {
    ///     let schema = schema_locator.event_schema(record).unwrap();
    ///     let provider_name = schema.provider_name();
    /// };
    /// ```
    /// [TraceEventInfo]: crate::native::tdh::TraceEventInfo
    pub fn provider_name(&self) -> String {
        self.te_info.provider_name()
    }

    /// Use the `task_name` function to obtain the Task name from the `TRACE_EVENT_INFO`
    ///
    /// See: [TaskType](https://docs.microsoft.com/en-us/windows/win32/wes/eventmanifestschema-tasktype-complextype)
    /// # Example
    /// ```
    /// # use ferrisetw::EventRecord;
    /// # use ferrisetw::schema_locator::SchemaLocator;
    /// let my_callback = |record: &EventRecord, schema_locator: &SchemaLocator| {
    ///     let schema = schema_locator.event_schema(record).unwrap();
    ///     let task_name = schema.task_name();
    /// };
    /// ```
    /// [TraceEventInfo]: crate::native::tdh::TraceEventInfo
    pub fn task_name(&self) -> String {
        self.te_info.task_name()
    }

    /// Use the `opcode_name` function to obtain the Opcode name from the `TRACE_EVENT_INFO`
    ///
    /// See: [OpcodeType](https://docs.microsoft.com/en-us/windows/win32/wes/eventmanifestschema-opcodetype-complextype)
    /// # Example
    /// ```
    /// # use ferrisetw::EventRecord;
    /// # use ferrisetw::schema_locator::SchemaLocator;
    /// let my_callback = |record: &EventRecord, schema_locator: &SchemaLocator| {
    ///     let schema = schema_locator.event_schema(record).unwrap();
    ///     let opcode_name = schema.opcode_name();
    /// };
    /// ```
    /// [TraceEventInfo]: crate::native::tdh::TraceEventInfo
    pub fn opcode_name(&self) -> String {
        self.te_info.opcode_name()
    }

    /// Parses the list of properties of the wrapped `TRACE_EVENT_INFO`
    ///
    /// This is parsed on first call, and cached for later use
    pub(crate) fn properties(&self) -> &[Property] {
        match self.try_properties() {
            Err(PropertyError::UnimplementedType(_)) => {
                log::error!("Unable to list properties: a type is not implemented");
                &[]
            }
            Ok(p) => p,
        }
    }

    pub(crate) fn try_properties(&self) -> Result<&[Property], PropertyError> {
        let cache = self.cached_properties.get_or_init(|| {
            let mut cache = Vec::new();
            for property in self.te_info.properties() {
                cache.push(property?)
            }
            Ok(cache)
        });

        match cache {
            Err(e) => Err(e.clone()),
            Ok(cache) => Ok(cache.as_slice()),
        }
    }
}

impl PartialEq for Schema {
    fn eq(&self, other: &Self) -> bool {
        self.te_info.event_id() == other.te_info.event_id()
            && self.te_info.provider_guid() == other.te_info.provider_guid()
            && self.te_info.event_version() == other.te_info.event_version()
    }
}

impl Eq for Schema {}
