use serde::{Deserialize, Serialize};
use static_assertions::assert_impl_all;
use std::convert::{TryFrom, TryInto};
use zbus_names::{InterfaceName, MemberName};
use zvariant::{derive::Type, ObjectPath};

use crate::{Error, Message, MessageField, MessageFieldCode, MessageHeader, Result};

// It's actually 10 (and even not that) but let's round it to next 8-byte alignment
const MAX_FIELDS_IN_MESSAGE: usize = 16;

/// A collection of [`MessageField`] instances.
///
/// [`MessageField`]: enum.MessageField.html
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct MessageFields<'m>(#[serde(borrow)] Vec<MessageField<'m>>);

assert_impl_all!(MessageFields<'_>: Send, Sync, Unpin);

impl<'m> MessageFields<'m> {
    /// Creates an empty collection of fields.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a [`MessageField`] to the collection of fields in the message.
    ///
    /// [`MessageField`]: enum.MessageField.html
    pub fn add<'f: 'm>(&mut self, field: MessageField<'f>) {
        self.0.push(field);
    }

    /// Replaces a [`MessageField`] from the collection of fields with one with the same code,
    /// returning the old value if present.
    ///
    /// [`MessageField`]: enum.MessageField.html
    pub fn replace<'f: 'm>(&mut self, field: MessageField<'f>) -> Option<MessageField<'m>> {
        let code = field.code();
        if let Some(found) = self.0.iter_mut().find(|f| f.code() == code) {
            return Some(std::mem::replace(found, field));
        }
        self.add(field);
        None
    }
    /// Returns a slice with all the [`MessageField`] in the message.
    ///
    /// [`MessageField`]: enum.MessageField.html
    pub fn get(&self) -> &[MessageField<'m>] {
        &self.0
    }

    /// Gets a reference to a specific [`MessageField`] by its code.
    ///
    /// Returns `None` if the message has no such field.
    ///
    /// [`MessageField`]: enum.MessageField.html
    pub fn get_field(&self, code: MessageFieldCode) -> Option<&MessageField<'m>> {
        self.0.iter().find(|f| f.code() == code)
    }

    /// Consumes the `MessageFields` and returns a specific [`MessageField`] by its code.
    ///
    /// Returns `None` if the message has no such field.
    ///
    /// [`MessageField`]: enum.MessageField.html
    pub fn into_field(self, code: MessageFieldCode) -> Option<MessageField<'m>> {
        for field in self.0 {
            if field.code() == code {
                return Some(field);
            }
        }

        None
    }
}

/// A byte range of a field in a Message, used in [`QuickMessageFields`].
///
/// Some invalid encodings (end = 0) are used to indicate "not cached" and "not present".
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct FieldPos {
    start: u32,
    end: u32,
}

impl FieldPos {
    pub fn new_unknown() -> Self {
        Self { start: 0, end: 0 }
    }

    pub fn new_not_present() -> Self {
        Self { start: 1, end: 0 }
    }

    pub fn build(msg_buf: &[u8], field_buf: &str) -> Option<Self> {
        let buf_start = msg_buf.as_ptr() as usize;
        let field_start = field_buf.as_ptr() as usize;
        let offset = field_start.checked_sub(buf_start)?;
        if offset <= msg_buf.len() && offset + field_buf.len() <= msg_buf.len() {
            Some(Self {
                start: offset.try_into().ok()?,
                end: (offset + field_buf.len()).try_into().ok()?,
            })
        } else {
            None
        }
    }

    pub fn new<T>(msg_buf: &[u8], field: Option<&T>) -> Self
    where
        T: std::ops::Deref<Target = str>,
    {
        match field {
            Some(value) => Self::build(msg_buf, value).unwrap_or_else(Self::new_unknown),
            None => Self::new_not_present(),
        }
    }

    /// Reassemble a previously cached field, or generate it with the fallback closure.
    pub fn read<'m, T>(
        &self,
        msg_buf: &'m [u8],
        fallback: impl FnOnce() -> Result<Option<T>>,
    ) -> Result<Option<T>>
    where
        T: TryFrom<&'m str>,
        T::Error: Into<Error>,
    {
        match self {
            Self { start: 0, end: 0 } => fallback(),
            Self { start: 1, end: 0 } => Ok(None),
            Self { start, end } => {
                let s = std::str::from_utf8(&msg_buf[(*start as usize)..(*end as usize)])
                    .expect("Invalid utf8 when reconstructing string");
                T::try_from(s).map(Some).map_err(Into::into)
            }
        }
    }
}

/// A cache of some commonly-used fields of the header of a Message.
#[derive(Debug, Default, Copy, Clone)]
pub(crate) struct QuickMessageFields {
    path: FieldPos,
    interface: FieldPos,
    member: FieldPos,
    reply_serial: Option<Option<u32>>,
}

impl QuickMessageFields {
    pub fn new(buf: &[u8], header: &MessageHeader<'_>) -> Result<Self> {
        Ok(Self {
            path: FieldPos::new(buf, header.path()?),
            interface: FieldPos::new(buf, header.interface()?),
            member: FieldPos::new(buf, header.member()?),
            reply_serial: Some(header.reply_serial()?),
        })
    }

    pub fn path<'m>(&self, msg: &'m Message) -> Result<Option<ObjectPath<'m>>> {
        self.path
            .read(msg.as_bytes(), || Ok(msg.header()?.path()?.cloned()))
    }

    pub fn interface<'m>(&self, msg: &'m Message) -> Result<Option<InterfaceName<'m>>> {
        self.interface
            .read(msg.as_bytes(), || Ok(msg.header()?.interface()?.cloned()))
    }

    pub fn member<'m>(&self, msg: &'m Message) -> Result<Option<MemberName<'m>>> {
        self.member
            .read(msg.as_bytes(), || Ok(msg.header()?.member()?.cloned()))
    }

    pub fn reply_serial(&self, msg: &Message) -> Result<Option<u32>> {
        self.reply_serial
            .map(Ok)
            .unwrap_or_else(|| msg.header()?.reply_serial())
    }
}

impl<'m> Default for MessageFields<'m> {
    fn default() -> Self {
        Self(Vec::with_capacity(MAX_FIELDS_IN_MESSAGE))
    }
}

impl<'m> std::ops::Deref for MessageFields<'m> {
    type Target = [MessageField<'m>];

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

#[cfg(test)]
mod tests {
    use super::{MessageField, MessageFields};

    #[test]
    fn test() {
        let mut mf = MessageFields::new();
        assert_eq!(mf.len(), 0);
        mf.add(MessageField::ReplySerial(42));
        assert_eq!(mf.len(), 1);
        mf.add(MessageField::ReplySerial(43));
        assert_eq!(mf.len(), 2);

        let mut mf = MessageFields::new();
        assert_eq!(mf.len(), 0);
        mf.replace(MessageField::ReplySerial(42));
        assert_eq!(mf.len(), 1);
        mf.replace(MessageField::ReplySerial(43));
        assert_eq!(mf.len(), 1);
    }
}
