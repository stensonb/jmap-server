pub mod changes;
pub mod json;
pub mod local_store;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    hash::Hash,
};

use changes::JMAPState;
use json::{JSONPointer, JSONValue};
use store::{AccountId, ChangeLogId, StoreError};

pub const JMAP_MAIL: u8 = 0;
pub const JMAP_MAILBOX: u8 = 1;
pub const JMAP_THREAD: u8 = 2;

pub type JMAPId = u64;

#[derive(Debug)]
pub enum JMAPError {
    InvalidArguments,
    RequestTooLarge,
    StateMismatch,
    AnchorNotFound,
    UnsupportedFilter,
    UnsupportedSort,
    InternalError(StoreError),
}

impl From<StoreError> for JMAPError {
    fn from(e: StoreError) -> Self {
        JMAPError::InternalError(e)
    }
}

pub type Result<T> = std::result::Result<T, JMAPError>;

pub trait JMAPIdSerialize {
    fn from_jmap_string(id: &str) -> Option<Self>
    where
        Self: Sized;
    fn to_jmap_string(&self) -> String;
}

impl JMAPIdSerialize for JMAPId {
    fn from_jmap_string(id: &str) -> Option<Self>
    where
        Self: Sized,
    {
        if id.as_bytes().get(0)? == &b'i' {
            JMAPId::from_str_radix(id.get(1..)?, 16).ok()?.into()
        } else {
            None
        }
    }

    fn to_jmap_string(&self) -> String {
        format!("i{:02x}", self)
    }
}

#[derive(Debug, Clone)]
pub struct JMAPQuery<T, U> {
    pub account_id: AccountId,
    pub filter: JMAPFilter<T>,
    pub sort: Vec<JMAPComparator<U>>,
    pub position: i32,
    pub anchor: Option<JMAPId>,
    pub anchor_offset: i32,
    pub limit: usize,
    pub calculate_total: bool,
}

#[derive(Debug, Clone)]
pub struct JMAPQueryChanges<T, U> {
    pub account_id: AccountId,
    pub filter: JMAPFilter<T>,
    pub sort: Vec<JMAPComparator<U>>,
    pub since_query_state: JMAPState,
    pub max_changes: usize,
    pub up_to_id: Option<JMAPId>,
    pub calculate_total: bool,
}

#[derive(Debug)]
pub struct JMAPQueryResponse {
    pub query_state: JMAPState,
    pub is_immutable: bool,
    pub total: usize,
    pub ids: Vec<JMAPId>,
}

#[derive(Debug)]
pub struct JMAPQueryChangesResponseItem {
    pub id: JMAPId,
    pub index: usize,
}

#[derive(Debug)]
pub struct JMAPQueryChangesResponse {
    pub old_query_state: JMAPState,
    pub new_query_state: JMAPState,
    pub total: usize,
    pub removed: Vec<JMAPId>,
    pub added: Vec<JMAPQueryChangesResponseItem>,
}

#[derive(Debug, Clone)]
pub struct JMAPComparator<T> {
    pub property: T,
    pub is_ascending: bool,
    pub collation: Option<String>,
}

impl<T> JMAPComparator<T> {
    pub fn ascending(property: T) -> Self {
        Self {
            property,
            is_ascending: true,
            collation: None,
        }
    }

    pub fn descending(property: T) -> Self {
        Self {
            property,
            is_ascending: false,
            collation: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum JMAPLogicalOperator {
    And,
    Or,
    Not,
}

#[derive(Debug, Clone)]
pub struct JMAPFilterOperator<T> {
    pub operator: JMAPLogicalOperator,
    pub conditions: Vec<JMAPFilter<T>>,
}

#[derive(Debug, Clone)]
pub enum JMAPFilter<T> {
    Condition(T),
    Operator(JMAPFilterOperator<T>),
    None,
}

impl<T> JMAPFilter<T> {
    pub fn condition(cond: T) -> Self {
        JMAPFilter::Condition(cond)
    }

    pub fn and(conditions: Vec<JMAPFilter<T>>) -> Self {
        JMAPFilter::Operator(JMAPFilterOperator {
            operator: JMAPLogicalOperator::And,
            conditions,
        })
    }

    pub fn or(conditions: Vec<JMAPFilter<T>>) -> Self {
        JMAPFilter::Operator(JMAPFilterOperator {
            operator: JMAPLogicalOperator::Or,
            conditions,
        })
    }

    pub fn not(conditions: Vec<JMAPFilter<T>>) -> Self {
        JMAPFilter::Operator(JMAPFilterOperator {
            operator: JMAPLogicalOperator::Not,
            conditions,
        })
    }
}

#[derive(Debug)]
pub struct JMAPChangesResponse {
    pub old_state: JMAPState,
    pub new_state: JMAPState,
    pub has_more_changes: bool,
    pub total_changes: usize,
    pub created: HashSet<ChangeLogId>,
    pub updated: HashSet<ChangeLogId>,
    pub destroyed: HashSet<ChangeLogId>,
}

pub type JMAPSetIdList<'x, T, U, V> = HashMap<T, HashMap<U, JSONValue<'x, V>>>;

#[derive(Debug)]
pub struct JMAPSet<'x, T>
where
    T: Hash + Eq + PartialEq,
{
    pub account_id: AccountId,
    pub if_in_state: Option<JMAPState>,
    pub create: Option<JMAPSetIdList<'x, Cow<'x, str>, Cow<'x, str>, T>>,
    pub update: Option<JMAPSetIdList<'x, JMAPId, JSONPointer<'x, T>, T>>,
    pub destroy: Option<Vec<JMAPId>>,
}

#[derive(Debug)]
pub enum JMAPSetErrorType {
    Forbidden,
    OverQuota,
    TooLarge,
    RateLimit,
    NotFound,
    InvalidPatch,
    WillDestroy,
    InvalidProperties,
    Singleton,
}

#[derive(Debug)]
pub struct JMAPSetError {
    pub error_type: JMAPSetErrorType,
    pub description: Option<String>,
    pub properties: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub struct JMAPSetResponse<'x, T>
where
    T: Hash + Eq + PartialEq,
{
    pub old_state: JMAPState,
    pub new_state: JMAPState,
    pub created: Option<HashMap<Cow<'x, str>, JSONValue<'x, T>>>,
    pub updated: Option<HashMap<JMAPId, JSONValue<'x, T>>>,
    pub destroyed: Option<Vec<JMAPId>>,
    pub not_created: Option<HashMap<Cow<'x, str>, JMAPSetError>>,
    pub not_updated: Option<HashMap<JMAPId, JMAPSetError>>,
    pub not_destroyed: Option<HashMap<JMAPId, JMAPSetError>>,
}

impl JMAPSetError {
    pub fn new(error_type: JMAPSetErrorType) -> Self {
        Self {
            error_type,
            description: None,
            properties: None,
        }
    }
    pub fn new_full(error_type: JMAPSetErrorType, description: String) -> Self {
        Self {
            error_type,
            description: description.into(),
            properties: None,
        }
    }
}

pub struct JMAPGet<T> {
    pub account_id: AccountId,
    pub ids: Option<Vec<JMAPId>>,
    pub properties: Option<Vec<T>>,
}

pub struct JMAPGetResponse<'x, T>
where
    T: Hash + Eq + PartialEq,
{
    pub state: JMAPState,
    pub list: JSONValue<'x, T>,
    pub not_found: Option<Vec<JMAPId>>,
}