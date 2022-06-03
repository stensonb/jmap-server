use std::{collections::HashMap, fmt};

use jmap::request::{ArgumentSerializer, MaybeIdReference};
use serde::{ser::SerializeMap, Deserialize, Serialize};

use super::{
    schema::{Filter, Mailbox, Property, Value},
    set::SetArguments,
};

// Property de/serialization
impl Serialize for Property {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}
struct PropertyVisitor;

impl<'de> serde::de::Visitor<'de> for PropertyVisitor {
    type Value = Property;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid JMAP Mailbox property")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(Property::parse(v))
    }
}

impl<'de> Deserialize<'de> for Property {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(PropertyVisitor)
    }
}

// Mailbox de/serialization
impl Serialize for Mailbox {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(self.properties.len().into())?;

        for (name, value) in &self.properties {
            match value {
                Value::Id { value } => map.serialize_entry(name, value)?,
                Value::Text { value } => map.serialize_entry(name, value)?,
                Value::Bool { value } => map.serialize_entry(name, value)?,
                Value::Number { value } => map.serialize_entry(name, value)?,
                Value::MailboxRights { value } => map.serialize_entry(name, value)?,
                Value::Null => map.serialize_entry(name, &None::<&str>)?,
                Value::ResultReference { value } => map.serialize_entry(name, value)?,
                Value::IdReference { value } => {
                    map.serialize_entry(name, &format!("#{}", value))?
                }
            }
        }

        map.end()
    }
}

struct MailboxVisitor;

impl<'de> serde::de::Visitor<'de> for MailboxVisitor {
    type Value = Mailbox;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid JMAP e-mail object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        let mut properties: HashMap<Property, Value> = HashMap::new();

        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "name" => {
                    properties.insert(
                        Property::Name,
                        if let Some(value) = map.next_value::<Option<String>>()? {
                            Value::Text { value }
                        } else {
                            Value::Null
                        },
                    );
                }
                "parentId" => {
                    properties.insert(
                        Property::ParentId,
                        if let Some(value) = map.next_value::<Option<MaybeIdReference>>()? {
                            match value {
                                MaybeIdReference::Value(value) => Value::Id { value },
                                MaybeIdReference::Reference(value) => Value::IdReference { value },
                            }
                        } else {
                            Value::Null
                        },
                    );
                }
                "role" => {
                    properties.insert(
                        Property::Role,
                        if let Some(value) = map.next_value::<Option<String>>()? {
                            Value::Text { value }
                        } else {
                            Value::Null
                        },
                    );
                }
                "sortOrder" => {
                    properties.insert(
                        Property::SortOrder,
                        if let Some(value) = map.next_value::<Option<u32>>()? {
                            Value::Number { value }
                        } else {
                            Value::Null
                        },
                    );
                }
                _ if key.starts_with('#') => {
                    if let Some(property) = key.get(1..) {
                        properties.insert(
                            Property::parse(property),
                            Value::ResultReference {
                                value: map.next_value()?,
                            },
                        );
                    }
                }
                _ => (),
            }
        }

        Ok(Mailbox { properties })
    }
}

impl<'de> Deserialize<'de> for Mailbox {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(MailboxVisitor)
    }
}

// Argument serializer
impl ArgumentSerializer for SetArguments {
    fn deserialize<'x: 'y, 'y>(
        &'y mut self,
        property: &'x str,
        value: &mut impl serde::de::MapAccess<'x>,
    ) -> Result<(), String> {
        if property == "onDestroyRemoveEmails" {
            self.on_destroy_remove_emails = value.next_value().map_err(|err| err.to_string())?;
        }
        Ok(())
    }
}

// Filter deserializer
struct FilterVisitor;

impl<'de> serde::de::Visitor<'de> for FilterVisitor {
    type Value = Filter;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid JMAP e-mail object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        Ok(
            match map
                .next_key::<&str>()?
                .ok_or_else(|| serde::de::Error::custom("Missing filter property"))?
            {
                "parentId" => Filter::ParentId {
                    value: map.next_value()?,
                },
                "name" => Filter::Name {
                    value: map.next_value()?,
                },
                "role" => Filter::Role {
                    value: map.next_value()?,
                },
                "hasAnyRole" => Filter::HasAnyRole {
                    value: map.next_value()?,
                },
                "isSubscribed" => Filter::IsSubscribed {
                    value: map.next_value()?,
                },
                unsupported => Filter::Unsupported {
                    value: unsupported.to_string(),
                },
            },
        )
    }
}

impl<'de> Deserialize<'de> for Filter {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(FilterVisitor)
    }
}