use std::{collections::HashMap, fmt::Display};

use jmap::{jmap_store::orm, request::ResultReference, types::jmap::JMAPId};
use serde::{Deserialize, Serialize};
use store::FieldId;

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct Mailbox {
    pub properties: HashMap<Property, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Value {
    Id { value: JMAPId },
    Text { value: String },
    Bool { value: bool },
    Number { value: u32 },
    MailboxRights { value: MailboxRights },
    ResultReference { value: ResultReference },
    IdReference { value: String },
    Null,
}

impl Default for Value {
    fn default() -> Self {
        Value::Null
    }
}

impl orm::Value for Value {
    fn index_as(&self) -> orm::IndexableValue {
        match self {
            Value::Id { value } => u64::from(value).into(),
            Value::Text { value } => value.to_string().into(),
            Value::Number { value } => (*value).into(),
            _ => orm::IndexableValue::Null,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            Value::Text { value } => value.is_empty(),
            Value::Null => true,
            _ => false,
        }
    }
}

impl Value {
    pub fn unwrap_text(self) -> Option<String> {
        match self {
            Value::Text { value } => Some(value),
            _ => None,
        }
    }

    pub fn unwrap_number(self) -> Option<u32> {
        match self {
            Value::Number { value } => Some(value),
            _ => None,
        }
    }

    pub fn unwrap_id(self) -> Option<JMAPId> {
        match self {
            Value::Id { value } => Some(value),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text { value } => Some(value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<u32> {
        match self {
            Value::Number { value } => Some(*value),
            _ => None,
        }
    }

    pub fn as_id(&self) -> Option<u64> {
        match self {
            Value::Id { value } => Some(value.into()),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailboxRights {
    #[serde(rename = "mayReadItems")]
    may_read_items: bool,

    #[serde(rename = "mayAddItems")]
    may_add_items: bool,

    #[serde(rename = "mayRemoveItems")]
    may_remove_items: bool,

    #[serde(rename = "maySetSeen")]
    may_set_seen: bool,

    #[serde(rename = "maySetKeywords")]
    may_set_keywords: bool,

    #[serde(rename = "mayCreateChild")]
    may_create_child: bool,

    #[serde(rename = "mayRename")]
    may_rename: bool,

    #[serde(rename = "mayDelete")]
    may_delete: bool,

    #[serde(rename = "maySubmit")]
    may_submit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
#[repr(u8)]
pub enum Property {
    Id = 0,
    Name = 1,
    ParentId = 2,
    Role = 3,
    SortOrder = 4,
    TotalEmails = 5,
    UnreadEmails = 6,
    TotalThreads = 7,
    UnreadThreads = 8,
    MyRights = 9,
    IsSubscribed = 10,
    Invalid = 11,
}

impl Display for Property {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Property::Id => write!(f, "id"),
            Property::Name => write!(f, "name"),
            Property::ParentId => write!(f, "parentId"),
            Property::Role => write!(f, "role"),
            Property::SortOrder => write!(f, "sortOrder"),
            Property::TotalEmails => write!(f, "totalEmails"),
            Property::UnreadEmails => write!(f, "unreadEmails"),
            Property::TotalThreads => write!(f, "totalThreads"),
            Property::UnreadThreads => write!(f, "unreadThreads"),
            Property::MyRights => write!(f, "myRights"),
            Property::IsSubscribed => write!(f, "isSubscribed"),
            Property::Invalid => Ok(()),
        }
    }
}

impl Property {
    pub fn parse(value: &str) -> Self {
        match value {
            "id" => Property::Id,
            "name" => Property::Name,
            "parentId" => Property::ParentId,
            "role" => Property::Role,
            "sortOrder" => Property::SortOrder,
            "isSubscribed" => Property::IsSubscribed,
            "totalEmails" => Property::TotalEmails,
            "unreadEmails" => Property::UnreadEmails,
            "totalThreads" => Property::TotalThreads,
            "unreadThreads" => Property::UnreadThreads,
            "myRights" => Property::MyRights,
            _ => Property::Invalid,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Filter {
    ParentId { value: Option<JMAPId> },
    Name { value: String },
    Role { value: Option<String> },
    HasAnyRole { value: bool },
    IsSubscribed { value: bool },
    Unsupported { value: String },
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "property")]
pub enum Comparator {
    #[serde(rename = "name")]
    Name,
    #[serde(rename = "sortOrder")]
    SortOrder,
    #[serde(rename = "parentId")]
    ParentId,
}

impl From<Property> for FieldId {
    fn from(field: Property) -> Self {
        field as FieldId
    }
}

impl From<FieldId> for Property {
    fn from(field: FieldId) -> Self {
        match field {
            0 => Property::Id,
            1 => Property::Name,
            2 => Property::ParentId,
            3 => Property::Role,
            4 => Property::SortOrder,
            5 => Property::TotalEmails,
            6 => Property::UnreadEmails,
            7 => Property::TotalThreads,
            8 => Property::UnreadThreads,
            9 => Property::MyRights,
            10 => Property::IsSubscribed,
            _ => Property::Invalid,
        }
    }
}

impl TryFrom<&str> for Property {
    type Error = ();

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match Property::parse(value) {
            Property::Invalid => Err(()),
            property => Ok(property),
        }
    }
}