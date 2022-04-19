use jmap::error::method::MethodError;
use jmap::error::set::SetErrorType;
use jmap::id::blob::BlobId;
use jmap::id::jmap::JMAPIdReference;
use jmap::id::JMAPIdSerialize;
use jmap::jmap_store::blob::JMAPBlobStore;
use jmap::jmap_store::changes::JMAPChanges;
use jmap::protocol::json::JSONValue;
use jmap::protocol::json_pointer::JSONPointer;
use jmap::request::set::SetRequest;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::raw::Raw;
use mail_builder::headers::text::Text;
use mail_builder::headers::url::URL;
use mail_builder::mime::{BodyPart, MimePart};
use mail_builder::MessageBuilder;
use std::collections::{BTreeMap, HashMap, HashSet};
use store::batch::{Document, WriteBatch};
use store::chrono::DateTime;
use store::field::{DefaultOptions, Options};
use store::roaring::RoaringBitmap;
use store::{AccountId, Collection, DocumentId, JMAPId, JMAPIdPrefix, JMAPStore, Store, Tag};

use crate::mail::import::JMAPMailImport;
use crate::mail::parse::get_message_blob;
use crate::mail::{
    HeaderName, Keyword, MailHeaderForm, MailHeaderProperty, MailProperties, MessageField,
};

pub struct MessageItem {
    pub blob: Vec<u8>,
    pub mailbox_ids: Vec<DocumentId>,
    pub keywords: Vec<Tag>,
    pub received_at: Option<i64>,
}

pub trait JMAPMailSet {
    fn mail_set(&self, request: SetRequest) -> jmap::Result<JSONValue>;
    fn build_message(
        &self,
        account: AccountId,
        fields: JSONValue,
        existing_mailboxes: &RoaringBitmap,
        created_ids: &HashMap<String, JSONValue>,
    ) -> Result<MessageItem, JSONValue>;
    fn import_body_structure<'x: 'y, 'y>(
        &'y self,
        account: AccountId,
        part: &'x JSONValue,
        body_values: Option<&'x HashMap<String, JSONValue>>,
    ) -> Result<MimePart, JSONValue>;
    fn import_body_part<'x: 'y, 'y>(
        &'y self,
        account: AccountId,
        part: &'x JSONValue,
        body_values: Option<&'x HashMap<String, JSONValue>>,
        implicit_type: Option<&'x str>,
        strict_implicit_type: bool,
    ) -> Result<(MimePart, Option<&'x Vec<JSONValue>>), JSONValue>;
    fn import_body_parts<'x: 'y, 'y>(
        &'y self,
        account: AccountId,
        parts: &'x JSONValue,
        body_values: Option<&'x HashMap<String, JSONValue>>,
        implicit_type: Option<&'x str>,
        strict_implicit_type: bool,
    ) -> Result<Vec<MimePart>, JSONValue>;
}

impl<T> JMAPMailSet for JMAPStore<T>
where
    T: for<'x> Store<'x> + 'static,
{
    fn mail_set(&self, request: SetRequest) -> jmap::Result<JSONValue> {
        let old_state = self.get_state(request.account_id, Collection::Mail)?;
        if let Some(if_in_state) = request.if_in_state {
            if old_state != if_in_state {
                return Err(MethodError::StateMismatch);
            }
        }

        let mut changes = WriteBatch::new(request.account_id, self.config.is_in_cluster);
        let document_ids = self
            .get_document_ids(request.account_id, Collection::Mail)?
            .unwrap_or_else(RoaringBitmap::new);
        let mut mailbox_ids = None;
        let mut response = HashMap::new();

        let mut created = HashMap::with_capacity(request.create.len());
        let mut not_created = HashMap::with_capacity(request.create.len());

        for (create_id, message_fields) in request.create {
            let mailbox_ids = if let Some(mailbox_ids) = &mailbox_ids {
                mailbox_ids
            } else {
                mailbox_ids = self
                    .get_document_ids(request.account_id, Collection::Mailbox)?
                    .unwrap_or_default()
                    .into();
                mailbox_ids.as_ref().unwrap()
            };

            match self.build_message(request.account_id, message_fields, mailbox_ids, &created) {
                Ok(import_item) => {
                    created.insert(
                        create_id,
                        self.mail_import_blob(
                            request.account_id,
                            import_item.blob,
                            import_item.mailbox_ids,
                            import_item.keywords,
                            import_item.received_at,
                        )?,
                    );
                }
                Err(err) => {
                    not_created.insert(create_id, err);
                }
            }
        }

        let mut updated = HashMap::with_capacity(request.update.len());
        let mut not_updated = HashMap::with_capacity(request.update.len());

        'main: for (jmap_id_str, properties) in request.update {
            let (jmap_id, properties) = if let (Some(jmap_id), Some(properties)) = (
                JMAPId::from_jmap_string(&jmap_id_str),
                properties.unwrap_object(),
            ) {
                (jmap_id, properties)
            } else {
                not_updated.insert(
                    jmap_id_str,
                    JSONValue::new_error(
                        SetErrorType::InvalidProperties,
                        "Failed to parse request.",
                    ),
                );
                continue 'main;
            };
            let document_id = jmap_id.get_document_id();
            if !document_ids.contains(document_id) {
                not_updated.insert(
                    jmap_id_str,
                    JSONValue::new_error(SetErrorType::NotFound, "ID not found."),
                );
                continue;
            } else if request
                .destroy
                .iter()
                .any(|x| x.to_string().map(|v| v == jmap_id_str).unwrap_or(false))
            {
                not_updated.insert(
                    jmap_id_str,
                    JSONValue::new_error(SetErrorType::WillDestroy, "ID will be destroyed."),
                );
                continue;
            }
            let mut document = Document::new(Collection::Mail, document_id);

            let mut keyword_op_list = HashMap::new();
            let mut keyword_op_clear_all = false;
            let mut mailbox_op_list = HashMap::new();
            let mut mailbox_op_clear_all = false;

            for (field, value) in properties {
                match JSONPointer::parse(&field).unwrap_or(JSONPointer::Root) {
                    JSONPointer::String(field) => {
                        match MailProperties::parse(&field).unwrap_or(MailProperties::Id) {
                            MailProperties::Keywords => {
                                if let JSONValue::Object(value) = value {
                                    // Add keywords to the list
                                    for (keyword, value) in value {
                                        if let JSONValue::Bool(true) = value {
                                            keyword_op_list
                                                .insert(Keyword::from_jmap(keyword), true);
                                        }
                                    }
                                    keyword_op_clear_all = true;
                                } else {
                                    not_updated.insert(
                                        jmap_id_str,
                                        JSONValue::new_invalid_property(
                                            "keywords",
                                            "Expected an object.",
                                        ),
                                    );
                                    continue 'main;
                                }
                            }
                            MailProperties::MailboxIds => {
                                // Unwrap JSON object
                                if let JSONValue::Object(value) = value {
                                    // Add mailbox ids to the list
                                    for (mailbox_id, value) in value {
                                        match (
                                            JMAPId::from_jmap_ref(mailbox_id.as_ref(), &created),
                                            value,
                                        ) {
                                            (Ok(mailbox_id), JSONValue::Bool(true)) => {
                                                mailbox_op_list.insert(
                                                    Tag::Id(mailbox_id.get_document_id()),
                                                    true,
                                                );
                                            }
                                            (Err(err), _) => {
                                                not_updated.insert(
                                                    jmap_id_str,
                                                    JSONValue::new_invalid_property(
                                                        format!("mailboxIds/{}", mailbox_id),
                                                        err.to_string(),
                                                    ),
                                                );
                                                continue 'main;
                                            }
                                            _ => (),
                                        }
                                    }
                                    mailbox_op_clear_all = true;
                                } else {
                                    // mailboxIds is not a JSON object
                                    not_updated.insert(
                                        jmap_id_str,
                                        JSONValue::new_invalid_property(
                                            "mailboxIds",
                                            "Expected an object.",
                                        ),
                                    );
                                    continue 'main;
                                }
                            }
                            _ => {
                                not_updated.insert(
                                    jmap_id_str,
                                    JSONValue::new_invalid_property(field, "Unsupported property."),
                                );
                                continue 'main;
                            }
                        }
                    }

                    JSONPointer::Path(mut path) if path.len() == 2 => {
                        if let (JSONPointer::String(property), JSONPointer::String(field)) =
                            (path.pop().unwrap(), path.pop().unwrap())
                        {
                            let is_mailbox =
                                match MailProperties::parse(&field).unwrap_or(MailProperties::Id) {
                                    MailProperties::MailboxIds => true,
                                    MailProperties::Keywords => false,
                                    _ => {
                                        not_updated.insert(
                                            jmap_id_str,
                                            JSONValue::new_invalid_property(
                                                format!("{}/{}", field, property),
                                                "Unsupported property.",
                                            ),
                                        );
                                        continue 'main;
                                    }
                                };
                            match value {
                                JSONValue::Null | JSONValue::Bool(false) => {
                                    if is_mailbox {
                                        match JMAPId::from_jmap_ref(property.as_ref(), &created) {
                                            Ok(mailbox_id) => {
                                                mailbox_op_list.insert(
                                                    Tag::Id(mailbox_id.get_document_id()),
                                                    false,
                                                );
                                            }
                                            Err(err) => {
                                                not_updated.insert(
                                                    jmap_id_str,
                                                    JSONValue::new_invalid_property(
                                                        format!("{}/{}", field, property),
                                                        err.to_string(),
                                                    ),
                                                );
                                                continue 'main;
                                            }
                                        }
                                    } else {
                                        keyword_op_list.insert(Keyword::from_jmap(property), false);
                                    }
                                }
                                JSONValue::Bool(true) => {
                                    if is_mailbox {
                                        match JMAPId::from_jmap_ref(property.as_ref(), &created) {
                                            Ok(mailbox_id) => {
                                                mailbox_op_list.insert(
                                                    Tag::Id(mailbox_id.get_document_id()),
                                                    true,
                                                );
                                            }
                                            Err(err) => {
                                                not_updated.insert(
                                                    jmap_id_str,
                                                    JSONValue::new_invalid_property(
                                                        format!("{}/{}", field, property),
                                                        err.to_string(),
                                                    ),
                                                );
                                                continue 'main;
                                            }
                                        }
                                    } else {
                                        keyword_op_list.insert(Keyword::from_jmap(property), true);
                                    }
                                }
                                _ => {
                                    not_updated.insert(
                                        jmap_id_str,
                                        JSONValue::new_invalid_property(
                                            format!("{}/{}", field, property),
                                            "Expected a boolean or null value.",
                                        ),
                                    );
                                    continue 'main;
                                }
                            }
                        } else {
                            not_updated.insert(
                                jmap_id_str,
                                JSONValue::new_invalid_property(field, "Unsupported property."),
                            );
                            continue 'main;
                        }
                    }
                    _ => {
                        not_updated.insert(
                            jmap_id_str,
                            JSONValue::new_invalid_property(
                                field.to_string(),
                                "Unsupported property.",
                            ),
                        );
                        continue 'main;
                    }
                }
            }

            let mut changed_mailboxes = HashSet::with_capacity(mailbox_op_list.len());
            if !mailbox_op_list.is_empty() || mailbox_op_clear_all {
                // Obtain mailboxes
                let mailbox_ids = if let Some(mailbox_ids) = &mailbox_ids {
                    mailbox_ids
                } else {
                    mailbox_ids = self
                        .get_document_ids(request.account_id, Collection::Mailbox)?
                        .unwrap_or_default()
                        .into();
                    mailbox_ids.as_ref().unwrap()
                };

                // Deserialize mailbox list
                let current_mailboxes = self
                    .get_document_tags(
                        request.account_id,
                        Collection::Mail,
                        document_id,
                        MessageField::Mailbox.into(),
                    )?
                    .map(|current_mailboxes| current_mailboxes.items)
                    .unwrap_or_default();

                let mut has_mailboxes = false;

                for mailbox in &current_mailboxes {
                    if mailbox_op_clear_all {
                        // Untag mailbox unless it is in the list of mailboxes to tag
                        if !mailbox_op_list.get(mailbox).unwrap_or(&false) {
                            document.tag(
                                MessageField::Mailbox,
                                mailbox.clone(),
                                DefaultOptions::new().clear(),
                            );
                            changed_mailboxes.insert(mailbox.unwrap_id().unwrap_or_default());
                        }
                    } else if !mailbox_op_list.get(mailbox).unwrap_or(&true) {
                        // Untag mailbox if is marked for untagging
                        document.tag(
                            MessageField::Mailbox,
                            mailbox.clone(),
                            DefaultOptions::new().clear(),
                        );
                        changed_mailboxes.insert(mailbox.unwrap_id().unwrap_or_default());
                    } else {
                        // Keep mailbox in the list
                        has_mailboxes = true;
                    }
                }

                for (mailbox, do_create) in mailbox_op_list {
                    if do_create {
                        let mailbox_id = mailbox.unwrap_id().unwrap_or_default();
                        // Make sure the mailbox exists
                        if mailbox_ids.contains(mailbox_id) {
                            // Tag mailbox if it is not already tagged
                            if !current_mailboxes.contains(&mailbox) {
                                document.tag(MessageField::Mailbox, mailbox, DefaultOptions::new());
                                changed_mailboxes.insert(mailbox_id);
                            }
                            has_mailboxes = true;
                        } else {
                            not_updated.insert(
                                jmap_id_str,
                                JSONValue::new_invalid_property(
                                    format!("mailboxIds/{}", mailbox_id),
                                    "Mailbox does not exist.",
                                ),
                            );
                            continue 'main;
                        }
                    }
                }

                // Messages have to be in at least one mailbox
                if !has_mailboxes {
                    not_updated.insert(
                        jmap_id_str,
                        JSONValue::new_invalid_property(
                            "mailboxIds",
                            "Message must belong to at least one mailbox.",
                        ),
                    );
                    continue 'main;
                }
            }

            if !keyword_op_list.is_empty() || keyword_op_clear_all {
                // Deserialize current keywords
                let current_keywords = self
                    .get_document_tags(
                        request.account_id,
                        Collection::Mail,
                        document_id,
                        MessageField::Keyword.into(),
                    )?
                    .map(|tags| tags.items)
                    .unwrap_or_default();

                let mut unread_changed = false;
                for keyword in &current_keywords {
                    if keyword_op_clear_all {
                        // Untag keyword unless it is in the list of keywords to tag
                        if !keyword_op_list.get(keyword).unwrap_or(&false) {
                            document.tag(
                                MessageField::Keyword,
                                keyword.clone(),
                                DefaultOptions::new().clear(),
                            );
                            if !unread_changed
                                && matches!(keyword, Tag::Static(k_id) if k_id == &Keyword::SEEN )
                            {
                                unread_changed = true;
                            }
                        }
                    } else if !keyword_op_list.get(keyword).unwrap_or(&true) {
                        // Untag keyword if is marked for untagging
                        document.tag(
                            MessageField::Keyword,
                            keyword.clone(),
                            DefaultOptions::new().clear(),
                        );
                        if !unread_changed
                            && matches!(keyword, Tag::Static(k_id) if k_id == &Keyword::SEEN )
                        {
                            unread_changed = true;
                        }
                    }
                }

                for (keyword, do_create) in keyword_op_list {
                    if do_create {
                        // Tag keyword if it is not already tagged
                        if !current_keywords.contains(&keyword) {
                            document.tag(
                                MessageField::Keyword,
                                keyword.clone(),
                                DefaultOptions::new(),
                            );
                            if !unread_changed
                                && matches!(&keyword, Tag::Static(k_id) if k_id == &Keyword::SEEN )
                            {
                                unread_changed = true;
                            }
                        }
                    }
                }

                // Mark mailboxes as changed if the message is tagged/untagged with $seen
                if unread_changed {
                    if let Some(current_mailboxes) = self.get_document_tags(
                        request.account_id,
                        Collection::Mail,
                        document_id,
                        MessageField::Mailbox.into(),
                    )? {
                        for mailbox in current_mailboxes.items {
                            changed_mailboxes.insert(mailbox.unwrap_id().unwrap_or_default());
                        }
                    }
                }
            }

            // Log mailbox changes
            if !changed_mailboxes.is_empty() {
                for changed_mailbox_id in changed_mailboxes {
                    changes.log_child_update(Collection::Mailbox, changed_mailbox_id);
                }
            }

            if !document.is_empty() {
                changes.update_document(document);
                changes.log_update(Collection::Mail, jmap_id);
                updated.insert(jmap_id_str, JSONValue::Null);
            } else {
                not_updated.insert(
                    jmap_id_str,
                    JSONValue::new_error(
                        SetErrorType::InvalidPatch,
                        "No changes found in request.",
                    ),
                );
            }
        }

        let mut destroyed = Vec::with_capacity(request.destroy.len());
        let mut not_destroyed = HashMap::with_capacity(request.destroy.len());

        for destroy_id in request.destroy {
            if let Some(jmap_id) = destroy_id.to_string() {
                if let Ok(jmap_id) = JMAPId::from_jmap_ref(jmap_id, &created) {
                    let document_id = jmap_id.get_document_id();
                    if document_ids.contains(document_id) {
                        changes.delete_document(Collection::Mail, document_id);
                        changes.log_delete(Collection::Mail, jmap_id);
                        destroyed.push(destroy_id);
                        continue;
                    }
                }
            }
            if let JSONValue::String(destroy_id) = destroy_id {
                not_destroyed.insert(
                    destroy_id,
                    JSONValue::new_error(SetErrorType::NotFound, "ID not found."),
                );
            }
        }

        response.insert("created".to_string(), created.into());
        response.insert("notCreated".to_string(), not_created.into());

        response.insert("updated".to_string(), updated.into());
        response.insert("notUpdated".to_string(), not_updated.into());

        response.insert("destroyed".to_string(), destroyed.into());
        response.insert("notDestroyed".to_string(), not_destroyed.into());

        response.insert(
            "newState".to_string(),
            if !changes.is_empty() {
                self.write(changes)?;
                self.get_state(request.account_id, Collection::Mail)?
            } else {
                old_state.clone()
            }
            .into(),
        );
        response.insert("oldState".to_string(), old_state.into());

        Ok(response.into())
    }

    #[allow(clippy::blocks_in_if_conditions)]
    fn build_message(
        &self,
        account: AccountId,
        fields: JSONValue,
        existing_mailboxes: &RoaringBitmap,
        created_ids: &HashMap<String, JSONValue>,
    ) -> Result<MessageItem, JSONValue> {
        let fields = if let JSONValue::Object(fields) = fields {
            fields
        } else {
            return Err(JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Failed to parse request.",
            ));
        };

        let body_values = fields.get("bodyValues").and_then(|v| v.to_object());

        let mut builder = MessageBuilder::new();
        let mut mailbox_ids: Vec<DocumentId> = Vec::new();
        let mut keywords: Vec<Tag> = Vec::new();
        let mut received_at: Option<i64> = None;

        for (property, value) in &fields {
            match MailProperties::parse(property).ok_or_else(|| {
                JSONValue::new_error(
                    SetErrorType::InvalidProperties,
                    format!("Failed to parse {}", property),
                )
            })? {
                MailProperties::MailboxIds => {
                    for (mailbox, value) in value.to_object().ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::InvalidProperties,
                            "Expected object containing mailboxIds",
                        )
                    })? {
                        let mailbox_id = JMAPId::from_jmap_ref(mailbox, created_ids)
                            .map_err(|err| {
                                JSONValue::new_error(
                                    SetErrorType::InvalidProperties,
                                    err.to_string(),
                                )
                            })?
                            .get_document_id();

                        if value.to_bool().ok_or_else(|| {
                            JSONValue::new_error(
                                SetErrorType::InvalidProperties,
                                "Expected boolean value in mailboxIds",
                            )
                        })? {
                            if !existing_mailboxes.contains(mailbox_id) {
                                return Err(JSONValue::new_error(
                                    SetErrorType::InvalidProperties,
                                    format!("mailboxId {} does not exist.", mailbox),
                                ));
                            }
                            mailbox_ids.push(mailbox_id);
                        }
                    }
                }
                MailProperties::Keywords => {
                    for (keyword, value) in value.to_object().ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::InvalidProperties,
                            "Expected object containing keywords",
                        )
                    })? {
                        if value.to_bool().ok_or_else(|| {
                            JSONValue::new_error(
                                SetErrorType::InvalidProperties,
                                "Expected boolean value in keywords",
                            )
                        })? {
                            keywords.push(Keyword::from_jmap(keyword.to_string()));
                        }
                    }
                }
                MailProperties::ReceivedAt => {
                    received_at = import_json_date(value)?.into();
                }
                MailProperties::MessageId => builder.header(
                    "Message-ID",
                    MessageId::from(import_json_string_list(value)?),
                ),
                MailProperties::InReplyTo => builder.header(
                    "In-Reply-To",
                    MessageId::from(import_json_string_list(value)?),
                ),
                MailProperties::References => builder.header(
                    "References",
                    MessageId::from(import_json_string_list(value)?),
                ),
                MailProperties::Sender => {
                    builder.header("Sender", Address::List(import_json_addresses(value)?))
                }
                MailProperties::From => {
                    builder.header("From", Address::List(import_json_addresses(value)?))
                }
                MailProperties::To => {
                    builder.header("To", Address::List(import_json_addresses(value)?))
                }
                MailProperties::Cc => {
                    builder.header("Cc", Address::List(import_json_addresses(value)?))
                }
                MailProperties::Bcc => {
                    builder.header("Bcc", Address::List(import_json_addresses(value)?))
                }
                MailProperties::ReplyTo => {
                    builder.header("Reply-To", Address::List(import_json_addresses(value)?))
                }
                MailProperties::Subject => {
                    builder.header("Subject", Text::new(import_json_string(value)?));
                }
                MailProperties::SentAt => {
                    builder.header("Date", Date::new(import_json_date(value)?))
                }
                MailProperties::TextBody => {
                    builder.text_body = self
                        .import_body_parts(account, value, body_values, "text/plain".into(), true)?
                        .pop()
                        .ok_or_else(|| {
                            JSONValue::new_error(
                                SetErrorType::InvalidProperties,
                                "No text body part found".to_string(),
                            )
                        })?
                        .into();
                }
                MailProperties::HtmlBody => {
                    builder.html_body = self
                        .import_body_parts(account, value, body_values, "text/html".into(), true)?
                        .pop()
                        .ok_or_else(|| {
                            JSONValue::new_error(
                                SetErrorType::InvalidProperties,
                                "No html body part found".to_string(),
                            )
                        })?
                        .into();
                }
                MailProperties::Attachments => {
                    builder.attachments = self
                        .import_body_parts(account, value, body_values, None, false)?
                        .into();
                }
                MailProperties::BodyStructure => {
                    builder.body = self
                        .import_body_structure(account, value, body_values)?
                        .into();
                }
                MailProperties::Header(MailHeaderProperty { form, header, all }) => {
                    if !all {
                        import_header(&mut builder, header, form, value)?;
                    } else {
                        for value in value.to_array().ok_or_else(|| {
                            JSONValue::new_error(
                                SetErrorType::InvalidProperties,
                                "Expected an array.".to_string(),
                            )
                        })? {
                            import_header(&mut builder, header.clone(), form.clone(), value)?;
                        }
                    }
                }

                MailProperties::Id
                | MailProperties::Size
                | MailProperties::BlobId
                | MailProperties::Preview
                | MailProperties::ThreadId
                | MailProperties::BodyValues
                | MailProperties::HasAttachment => (),
            }
        }

        if mailbox_ids.is_empty() {
            return Err(JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Message has to belong to at least one mailbox.",
            ));
        }

        if builder.headers.is_empty()
            && builder.body.is_none()
            && builder.html_body.is_none()
            && builder.text_body.is_none()
            && builder.attachments.is_none()
        {
            return Err(JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Message has to have at least one header or body part.",
            ));
        }

        // TODO: write parsed message directly to store, avoid parsing it again.
        let mut blob = Vec::with_capacity(1024);
        builder
            .write_to(&mut blob)
            .map_err(|_| JSONValue::new_error(SetErrorType::InvalidProperties, "Internal error"))?;

        Ok(MessageItem {
            blob,
            mailbox_ids,
            keywords,
            received_at,
        })
    }

    fn import_body_structure<'x: 'y, 'y>(
        &'y self,
        account: AccountId,
        part: &'x JSONValue,
        body_values: Option<&'x HashMap<String, JSONValue>>,
    ) -> Result<MimePart, JSONValue> {
        let (mut mime_part, sub_parts) =
            self.import_body_part(account, part, body_values, None, false)?;

        if let Some(sub_parts) = sub_parts {
            let mut stack = Vec::new();
            let mut it = sub_parts.iter();

            loop {
                while let Some(part) = it.next() {
                    let (sub_mime_part, sub_parts) =
                        self.import_body_part(account, part, body_values, None, false)?;
                    if let Some(sub_parts) = sub_parts {
                        stack.push((mime_part, it));
                        mime_part = sub_mime_part;
                        it = sub_parts.iter();
                    } else {
                        mime_part.add_part(sub_mime_part);
                    }
                }
                if let Some((mut prev_mime_part, prev_it)) = stack.pop() {
                    prev_mime_part.add_part(mime_part);
                    mime_part = prev_mime_part;
                    it = prev_it;
                } else {
                    break;
                }
            }
        }

        Ok(mime_part)
    }

    fn import_body_part<'x: 'y, 'y>(
        &'y self,
        account: AccountId,
        part: &'x JSONValue,
        body_values: Option<&'x HashMap<String, JSONValue>>,
        implicit_type: Option<&'x str>,
        strict_implicit_type: bool,
    ) -> Result<(MimePart, Option<&'x Vec<JSONValue>>), JSONValue> {
        let part = part.to_object().ok_or_else(|| {
            JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Expected an object in body part list.".to_string(),
            )
        })?;

        let content_type = part
            .get("type")
            .and_then(|v| v.to_string())
            .unwrap_or_else(|| implicit_type.unwrap_or("text/plain"));

        if strict_implicit_type && implicit_type.unwrap() != content_type {
            return Err(JSONValue::new_error(
                SetErrorType::InvalidProperties,
                format!(
                    "Expected exactly body part of type \"{}\"",
                    implicit_type.unwrap()
                ),
            ));
        }

        let is_multipart = content_type.starts_with("multipart/");
        let mut mime_part = MimePart {
            headers: BTreeMap::new(),
            contents: if is_multipart {
                BodyPart::Multipart(vec![])
            } else if let Some(part_id) = part.get("partId").and_then(|v| v.to_string()) {
                BodyPart::Text( body_values
                    .ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::InvalidProperties,
                            "Missing \"bodyValues\" object containing partId.".to_string(),
                        )
                    })?
                    .get(part_id)
                    .ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::InvalidProperties,
                            format!("Missing body value for partId \"{}\"", part_id),
                        )
                    })?
                    .to_object()
                    .ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::InvalidProperties,
                            format!("Expected a bodyValues object defining partId \"{}\"", part_id),
                        )
                    })?
                    .get("value")
                    .ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::InvalidProperties,
                            format!("Missing \"value\" field in bodyValues object defining partId \"{}\"", part_id),
                        )
                    })?
                    .to_string()
                    .ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::InvalidProperties,
                            format!("Expected a string \"value\" field in bodyValues object defining partId \"{}\"", part_id),
                        )
                    })?.into())
            } else if let Some(blob_id) = part.get("blobId").and_then(|v| v.to_string()) {
                BodyPart::Binary(
                    self.download_blob(
                        account,
                        &BlobId::from_jmap_string(blob_id).ok_or_else(|| {
                            JSONValue::new_error(
                                SetErrorType::BlobNotFound,
                                "Failed to parse blobId",
                            )
                        })?,
                        get_message_blob,
                    )
                    .map_err(|_| {
                        JSONValue::new_error(SetErrorType::BlobNotFound, "Failed to fetch blob.")
                    })?
                    .ok_or_else(|| {
                        JSONValue::new_error(
                            SetErrorType::BlobNotFound,
                            "blobId does not exist on this server.",
                        )
                    })?
                    .into(),
                )
            } else {
                return Err(JSONValue::new_error(
                    SetErrorType::InvalidProperties,
                    "Expected a \"partId\" or \"blobId\" field in body part.".to_string(),
                ));
            },
        };

        let mut content_type = ContentType::new(content_type);
        if !is_multipart {
            if content_type.c_type.starts_with("text/") {
                if matches!(mime_part.contents, BodyPart::Text(_)) {
                    content_type
                        .attributes
                        .insert("charset".into(), "utf-8".into());
                } else if let Some(charset) = part.get("charset") {
                    content_type.attributes.insert(
                        "charset".into(),
                        charset
                            .to_string()
                            .ok_or_else(|| {
                                JSONValue::new_error(
                                    SetErrorType::InvalidProperties,
                                    "Expected a string value for \"charset\" field.".to_string(),
                                )
                            })?
                            .into(),
                    );
                };
            }

            match (
                part.get("disposition").and_then(|v| v.to_string()),
                part.get("name").and_then(|v| v.to_string()),
            ) {
                (Some(disposition), Some(filename)) => {
                    mime_part.headers.insert(
                        "Content-Disposition".into(),
                        ContentType::new(disposition)
                            .attribute("filename", filename)
                            .into(),
                    );
                }
                (Some(disposition), None) => {
                    mime_part.headers.insert(
                        "Content-Disposition".into(),
                        ContentType::new(disposition).into(),
                    );
                }
                (None, Some(filename)) => {
                    content_type
                        .attributes
                        .insert("name".into(), filename.into());
                }
                (None, None) => (),
            };

            if let Some(languages) = part.get("language").and_then(|v| v.to_array()) {
                mime_part.headers.insert(
                    "Content-Language".into(),
                    Text::new(
                        languages
                            .iter()
                            .filter_map(|v| v.to_string())
                            .collect::<Vec<&str>>()
                            .join(","),
                    )
                    .into(),
                );
            }

            if let Some(cid) = part.get("cid").and_then(|v| v.to_string()) {
                mime_part
                    .headers
                    .insert("Content-ID".into(), MessageId::new(cid).into());
            }

            if let Some(location) = part.get("location").and_then(|v| v.to_string()) {
                mime_part
                    .headers
                    .insert("Content-Location".into(), Text::new(location).into());
            }
        }

        mime_part
            .headers
            .insert("Content-Type".into(), content_type.into());

        for (property, value) in part {
            if property.starts_with("header:") {
                match property.split(':').nth(1) {
                    Some(header_name) if !header_name.is_empty() => {
                        mime_part.headers.insert(
                            header_name.into(),
                            Raw::new(value.to_string().ok_or_else(|| {
                                JSONValue::new_error(
                                    SetErrorType::InvalidProperties,
                                    format!("Expected a string value for \"{}\" field.", property),
                                )
                            })?)
                            .into(),
                        );
                    }
                    _ => (),
                }
            }
        }

        if let Some(headers) = part.get("headers").and_then(|v| v.to_array()) {
            for header in headers {
                let header = header.to_object().ok_or_else(|| {
                    JSONValue::new_error(
                        SetErrorType::InvalidProperties,
                        "Expected an object for \"headers\" field.".to_string(),
                    )
                })?;
                mime_part.headers.insert(
                    header
                        .get("name")
                        .and_then(|v| v.to_string())
                        .ok_or_else(|| {
                            JSONValue::new_error(
                                SetErrorType::InvalidProperties,
                                "Expected a string value for \"name\" field in \"headers\" field."
                                    .to_string(),
                            )
                        })?
                        .into(),
                    Raw::new(
                        header
                            .get("value")
                            .and_then(|v| v.to_string())
                            .ok_or_else(|| {
                                JSONValue::new_error(
                                SetErrorType::InvalidProperties,
                                "Expected a string value for \"value\" field in \"headers\" field."
                                    .to_string(),
                            )
                            })?,
                    )
                    .into(),
                );
            }
        }
        Ok((
            mime_part,
            if is_multipart {
                part.get("subParts").and_then(|v| v.to_array())
            } else {
                None
            },
        ))
    }

    fn import_body_parts<'x: 'y, 'y>(
        &'y self,
        account: AccountId,
        parts: &'x JSONValue,
        body_values: Option<&'x HashMap<String, JSONValue>>,
        implicit_type: Option<&'x str>,
        strict_implicit_type: bool,
    ) -> Result<Vec<MimePart>, JSONValue> {
        let parts = parts.to_array().ok_or_else(|| {
            JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Expected an array containing body part.".to_string(),
            )
        })?;

        let mut result = Vec::with_capacity(parts.len());
        for part in parts {
            result.push(
                self.import_body_part(
                    account,
                    part,
                    body_values,
                    implicit_type,
                    strict_implicit_type,
                )?
                .0,
            );
        }

        Ok(result)
    }
}

fn import_header<'y>(
    builder: &mut MessageBuilder<'y>,
    header: HeaderName,
    form: MailHeaderForm,
    value: &'y JSONValue,
) -> Result<(), JSONValue> {
    match form {
        MailHeaderForm::Raw => {
            builder.header(header.unwrap(), Raw::new(import_json_string(value)?))
        }
        MailHeaderForm::Text => {
            builder.header(header.unwrap(), Text::new(import_json_string(value)?))
        }
        MailHeaderForm::Addresses => builder.header(
            header.unwrap(),
            Address::List(import_json_addresses(value)?),
        ),
        MailHeaderForm::GroupedAddresses => builder.header(
            header.unwrap(),
            Address::List(import_json_grouped_addresses(value)?),
        ),
        MailHeaderForm::MessageIds => builder.header(
            header.unwrap(),
            MessageId::from(import_json_string_list(value)?),
        ),
        MailHeaderForm::Date => {
            builder.header(header.unwrap(), Date::new(import_json_date(value)?))
        }
        MailHeaderForm::URLs => {
            builder.header(header.unwrap(), URL::from(import_json_string_list(value)?))
        }
    }
    Ok(())
}

fn import_json_string(value: &JSONValue) -> Result<&str, JSONValue> {
    value.to_string().ok_or_else(|| {
        JSONValue::new_error(
            SetErrorType::InvalidProperties,
            "Expected a String property.".to_string(),
        )
    })
}

fn import_json_date(value: &JSONValue) -> Result<i64, JSONValue> {
    Ok(
        DateTime::parse_from_rfc3339(value.to_string().ok_or_else(|| {
            JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Expected a Date property.".to_string(),
            )
        })?)
        .map_err(|_| {
            JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Expected a valid Date property.".to_string(),
            )
        })?
        .timestamp(),
    )
}

fn import_json_string_list(value: &JSONValue) -> Result<Vec<&str>, JSONValue> {
    let value = value.to_array().ok_or_else(|| {
        JSONValue::new_error(
            SetErrorType::InvalidProperties,
            "Expected an array with String.".to_string(),
        )
    })?;

    let mut list = Vec::with_capacity(value.len());
    for v in value {
        list.push(v.to_string().ok_or_else(|| {
            JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Expected an array with String.".to_string(),
            )
        })?);
    }

    Ok(list)
}

fn import_json_addresses(value: &JSONValue) -> Result<Vec<Address>, JSONValue> {
    let value = value.to_array().ok_or_else(|| {
        JSONValue::new_error(
            SetErrorType::InvalidProperties,
            "Expected an array with EmailAddress objects.".to_string(),
        )
    })?;

    let mut result = Vec::with_capacity(value.len());
    for addr in value {
        let addr = addr.to_object().ok_or_else(|| {
            JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Expected an array containing EmailAddress objects.".to_string(),
            )
        })?;
        result.push(Address::new_address(
            addr.get("name").and_then(|n| n.to_string()),
            addr.get("email")
                .and_then(|n| n.to_string())
                .ok_or_else(|| {
                    JSONValue::new_error(
                        SetErrorType::InvalidProperties,
                        "Missing 'email' field in EmailAddress object.".to_string(),
                    )
                })?,
        ));
    }

    Ok(result)
}

fn import_json_grouped_addresses(value: &JSONValue) -> Result<Vec<Address>, JSONValue> {
    let value = value.to_array().ok_or_else(|| {
        JSONValue::new_error(
            SetErrorType::InvalidProperties,
            "Expected an array with EmailAddressGroup objects.".to_string(),
        )
    })?;

    let mut result = Vec::with_capacity(value.len());
    for addr in value {
        let addr = addr.to_object().ok_or_else(|| {
            JSONValue::new_error(
                SetErrorType::InvalidProperties,
                "Expected an array containing EmailAddressGroup objects.".to_string(),
            )
        })?;
        result.push(Address::new_group(
            addr.get("name").and_then(|n| n.to_string()),
            import_json_addresses(addr.get("addresses").ok_or_else(|| {
                JSONValue::new_error(
                    SetErrorType::InvalidProperties,
                    "Missing 'addresses' field in EmailAddressGroup object.".to_string(),
                )
            })?)?,
        ));
    }

    Ok(result)
}