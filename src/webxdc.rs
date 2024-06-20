//! # Handle webxdc messages.
//!
//! Internally status updates are stored in the `msgs_status_updates` SQL table.
//! `msgs_status_updates` contains the following columns:
//! - `id` - status update serial number
//! - `msg_id` - ID of the message in the `msgs` table
//! - `update_item` - JSON representation of the status update
//! - `uid` - "id" field of the update, used for deduplication
//!
//! Status updates are scheduled for sending by adding a record
//! to `smtp_status_updates_table` SQL table.
//! `smtp_status_updates` contains the following columns:
//! - `msg_id` - ID of the message in the `msgs` table
//! - `first_serial` - serial number of the first status update to send
//! - `last_serial` - serial number of the last status update to send
//! - `descr` - text to send along with the updates

mod integration;
mod maps_integration;

use std::path::Path;

use anyhow::{anyhow, bail, ensure, format_err, Context as _, Result};

use deltachat_contact_tools::strip_rtlo_characters;
use deltachat_derive::FromSql;
use lettre_email::PartBuilder;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncReadExt;

use crate::chat::{self, Chat};
use crate::constants::Chattype;
use crate::contact::ContactId;
use crate::context::Context;
use crate::events::EventType;
use crate::message::{Message, MessageState, MsgId, Viewtype};
use crate::mimefactory::wrapped_base64_encode;
use crate::mimeparser::SystemMessage;
use crate::param::Param;
use crate::param::Params;
use crate::tools::create_id;
use crate::tools::{create_smeared_timestamp, get_abs_path};

/// The current API version.
/// If `min_api` in manifest.toml is set to a larger value,
/// the Webxdc's index.html is replaced by an error message.
/// In the future, that may be useful to avoid new Webxdc being loaded on old Delta Chats.
const WEBXDC_API_VERSION: u32 = 1;

/// Suffix used to recognize webxdc files.
pub const WEBXDC_SUFFIX: &str = "xdc";
const WEBXDC_DEFAULT_ICON: &str = "__webxdc__/default-icon.png";

/// Raw information read from manifest.toml
#[derive(Debug, Deserialize, Default)]
#[non_exhaustive]
pub struct WebxdcManifest {
    /// Webxdc name, used on icons or page titles.
    pub name: Option<String>,

    /// Minimum API version required to run this webxdc.
    pub min_api: Option<u32>,

    /// Optional URL of webxdc source code.
    pub source_code_url: Option<String>,

    /// Set to "map" to request integration.
    pub request_integration: Option<String>,

    /// If the webxdc requests network access.
    pub request_internet_access: Option<bool>,
}

/// Parsed information from WebxdcManifest and fallbacks.
#[derive(Debug, Serialize)]
pub struct WebxdcInfo {
    /// The name of the app.
    /// Defaults to filename if not set in the manifest.
    pub name: String,

    /// Filename of the app icon.
    pub icon: String,

    /// If the webxdc represents a document and allows to edit it,
    /// this is the document name.
    /// Otherwise an empty string.
    pub document: String,

    /// Short description of the webxdc state.
    /// For example, "7 votes".
    pub summary: String,

    /// URL of webxdc source code or an empty string.
    pub source_code_url: String,

    /// Set to "map" to request integration, otherwise an empty string.
    pub request_integration: String,

    /// If the webxdc is allowed to access the network.
    /// It should request access, be encrypted
    /// and sent to self for this.
    pub internet_access: bool,
}

/// Status Update ID.
#[derive(
    Debug,
    Copy,
    Clone,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    FromSql,
    FromPrimitive,
)]
pub struct StatusUpdateSerial(u32);

impl StatusUpdateSerial {
    /// Create a new [StatusUpdateSerial].
    pub fn new(id: u32) -> StatusUpdateSerial {
        StatusUpdateSerial(id)
    }

    /// Gets StatusUpdateSerial as untyped integer.
    /// Avoid using this outside ffi.
    pub fn to_u32(self) -> u32 {
        self.0
    }
}

impl rusqlite::types::ToSql for StatusUpdateSerial {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput> {
        let val = rusqlite::types::Value::Integer(i64::from(self.0));
        let out = rusqlite::types::ToSqlOutput::Owned(val);
        Ok(out)
    }
}

// Array of update items as sent on the wire.
#[derive(Debug, Deserialize)]
struct StatusUpdates {
    updates: Vec<StatusUpdateItem>,
}

/// Update items as sent on the wire and as stored in the database.
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct StatusUpdateItem {
    /// The playload of the status update.
    pub payload: Value,

    /// Optional short info message that will be displayed in the chat.
    /// For example "Alice added an item" or "Bob voted for option x".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info: Option<String>,

    /// The new name of the editing document.
    /// This is not needed if the webxdc doesn't edit documents.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document: Option<String>,

    /// Optional summary of the status update which will be shown next to the
    /// app icon. This should be short and can be something like "8 votes"
    /// for a voting app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,

    /// Unique ID for deduplication.
    /// This can be used if the message is sent over multiple transports.
    ///
    /// If there is no ID, message is always considered to be unique.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
}

/// Update items as passed to the UIs.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StatusUpdateItemAndSerial {
    #[serde(flatten)]
    item: StatusUpdateItem,

    serial: StatusUpdateSerial,
    max_serial: StatusUpdateSerial,
}

/// Returns an entry index and a reference.
fn find_zip_entry<'a>(
    file: &'a async_zip::ZipFile,
    name: &str,
) -> Option<(usize, &'a async_zip::StoredZipEntry)> {
    for (i, ent) in file.entries().iter().enumerate() {
        if ent.entry().filename() == name {
            return Some((i, ent));
        }
    }
    None
}

impl Context {
    /// check if a file is an acceptable webxdc for sending or receiving.
    pub(crate) async fn is_webxdc_file(&self, filename: &str, file: &[u8]) -> Result<bool> {
        if !filename.ends_with(WEBXDC_SUFFIX) {
            return Ok(false);
        }

        let archive = match async_zip::read::mem::ZipFileReader::new(file.to_vec()).await {
            Ok(archive) => archive,
            Err(_) => {
                info!(self, "{} cannot be opened as zip-file", &filename);
                return Ok(false);
            }
        };

        if find_zip_entry(archive.file(), "index.html").is_none() {
            info!(self, "{} misses index.html", &filename);
            return Ok(false);
        }

        Ok(true)
    }

    /// Ensure that a file is an acceptable webxdc for sending.
    pub(crate) async fn ensure_sendable_webxdc_file(&self, path: &Path) -> Result<()> {
        let filename = path.to_str().unwrap_or_default();
        if !filename.ends_with(WEBXDC_SUFFIX) {
            bail!("{} is not a valid webxdc file", filename);
        }

        let valid = match async_zip::read::fs::ZipFileReader::new(path).await {
            Ok(archive) => {
                if find_zip_entry(archive.file(), "index.html").is_none() {
                    warn!(self, "{} misses index.html", filename);
                    false
                } else {
                    true
                }
            }
            Err(_) => {
                warn!(self, "{} cannot be opened as zip-file", filename);
                false
            }
        };

        if !valid {
            bail!("{} is not a valid webxdc file", filename);
        }

        Ok(())
    }

    /// Check if the last message of a chat is an info message belonging to the given instance and sender.
    /// If so, the id of this message is returned.
    async fn get_overwritable_info_msg_id(
        &self,
        instance: &Message,
        from_id: ContactId,
    ) -> Result<Option<MsgId>> {
        if let Some((last_msg_id, last_from_id, last_param, last_in_repl_to)) = self
            .sql
            .query_row_optional(
                r#"SELECT id, from_id, param, mime_in_reply_to
                    FROM msgs
                    WHERE chat_id=?1 AND hidden=0
                    ORDER BY timestamp DESC, id DESC LIMIT 1"#,
                (instance.chat_id,),
                |row| {
                    let last_msg_id: MsgId = row.get(0)?;
                    let last_from_id: ContactId = row.get(1)?;
                    let last_param: Params = row.get::<_, String>(2)?.parse().unwrap_or_default();
                    let last_in_repl_to: String = row.get(3)?;
                    Ok((last_msg_id, last_from_id, last_param, last_in_repl_to))
                },
            )
            .await?
        {
            if last_from_id == from_id
                && last_param.get_cmd() == SystemMessage::WebxdcInfoMessage
                && last_in_repl_to == instance.rfc724_mid
            {
                return Ok(Some(last_msg_id));
            }
        }
        Ok(None)
    }

    /// Takes an update-json as `{payload: PAYLOAD}`
    /// writes it to the database and handles events, info-messages, document name and summary.
    async fn create_status_update_record(
        &self,
        instance: &Message,
        status_update_item: StatusUpdateItem,
        timestamp: i64,
        can_info_msg: bool,
        from_id: ContactId,
    ) -> Result<Option<StatusUpdateSerial>> {
        let Some(status_update_serial) = self
            .write_status_update_inner(&instance.id, &status_update_item, timestamp)
            .await?
        else {
            return Ok(None);
        };

        if can_info_msg {
            if let Some(ref info) = status_update_item.info {
                if let Some(info_msg_id) =
                    self.get_overwritable_info_msg_id(instance, from_id).await?
                {
                    chat::update_msg_text_and_timestamp(
                        self,
                        instance.chat_id,
                        info_msg_id,
                        info.as_str(),
                        timestamp,
                    )
                    .await?;
                } else {
                    chat::add_info_msg_with_cmd(
                        self,
                        instance.chat_id,
                        info.as_str(),
                        SystemMessage::WebxdcInfoMessage,
                        timestamp,
                        None,
                        Some(instance),
                        Some(from_id),
                    )
                    .await?;
                }
            }
        }

        let mut param_changed = false;

        let mut instance = instance.clone();
        if let Some(ref document) = status_update_item.document {
            if instance
                .param
                .update_timestamp(Param::WebxdcDocumentTimestamp, timestamp)?
            {
                instance.param.set(Param::WebxdcDocument, document);
                param_changed = true;
            }
        }

        if let Some(ref summary) = status_update_item.summary {
            if instance
                .param
                .update_timestamp(Param::WebxdcSummaryTimestamp, timestamp)?
            {
                instance
                    .param
                    .set(Param::WebxdcSummary, strip_rtlo_characters(summary));
                param_changed = true;
            }
        }

        if param_changed {
            instance.update_param(self).await?;
            self.emit_msgs_changed(instance.chat_id, instance.id);
        }

        if instance.viewtype == Viewtype::Webxdc {
            self.emit_event(EventType::WebxdcStatusUpdate {
                msg_id: instance.id,
                status_update_serial,
            });
        }

        Ok(Some(status_update_serial))
    }

    /// Inserts a status update item into `msgs_status_updates` table.
    ///
    /// Returns serial ID of the status update if a new item is inserted.
    pub(crate) async fn write_status_update_inner(
        &self,
        instance_id: &MsgId,
        status_update_item: &StatusUpdateItem,
        timestamp: i64,
    ) -> Result<Option<StatusUpdateSerial>> {
        let uid = status_update_item.uid.as_deref();
        let status_update_item = serde_json::to_string(&status_update_item)?;
        let trans_fn = |t: &mut rusqlite::Transaction| {
            t.execute(
                "UPDATE msgs SET timestamp_rcvd=? WHERE id=?",
                (timestamp, instance_id),
            )?;
            let rowid = t
                .query_row(
                    "INSERT INTO msgs_status_updates (msg_id, update_item, uid) VALUES(?, ?, ?)
                     ON CONFLICT (uid) DO NOTHING
                     RETURNING id",
                    (instance_id, status_update_item, uid),
                    |row| {
                        let id: u32 = row.get(0)?;
                        Ok(id)
                    },
                )
                .optional()?;
            Ok(rowid)
        };
        let Some(rowid) = self.sql.transaction(trans_fn).await? else {
            let uid = uid.unwrap_or("-");
            info!(self, "Ignoring duplicate status update with uid={uid}");
            return Ok(None);
        };
        let status_update_serial = StatusUpdateSerial(rowid);
        Ok(Some(status_update_serial))
    }

    /// Returns the update_item with `status_update_serial` from the webxdc with message id `msg_id`.
    pub async fn get_status_update(
        &self,
        msg_id: MsgId,
        status_update_serial: StatusUpdateSerial,
    ) -> Result<String> {
        self.sql
            .query_get_value(
                "SELECT update_item FROM msgs_status_updates WHERE id=? AND msg_id=? ",
                (status_update_serial.0, msg_id),
            )
            .await?
            .context("get_status_update: no update item found.")
    }

    /// Sends a status update for an webxdc instance.
    ///
    /// If the instance is a draft,
    /// the status update is sent once the instance is actually sent.
    /// Otherwise, the update is sent as soon as possible.
    pub async fn send_webxdc_status_update(
        &self,
        instance_msg_id: MsgId,
        update_str: &str,
        descr: &str,
    ) -> Result<()> {
        let status_update_item: StatusUpdateItem = serde_json::from_str(update_str)
            .with_context(|| format!("Failed to parse webxdc update item from {update_str:?}"))?;
        self.send_webxdc_status_update_struct(instance_msg_id, status_update_item, descr)
            .await?;
        Ok(())
    }

    /// Sends a status update for an webxdc instance.
    /// Also see [Self::send_webxdc_status_update]
    pub async fn send_webxdc_status_update_struct(
        &self,
        instance_msg_id: MsgId,
        mut status_update: StatusUpdateItem,
        descr: &str,
    ) -> Result<()> {
        let instance = Message::load_from_db(self, instance_msg_id)
            .await
            .with_context(|| {
                format!("Failed to load message {instance_msg_id} from the database")
            })?;
        let viewtype = instance.viewtype;
        if viewtype != Viewtype::Webxdc {
            bail!("send_webxdc_status_update: message {instance_msg_id} is not a webxdc message, but a {viewtype} message.");
        }

        if instance.param.get_int(Param::WebxdcIntegration).is_some() {
            return self
                .intercept_send_webxdc_status_update(instance, status_update)
                .await;
        }

        let chat_id = instance.chat_id;
        let chat = Chat::load_from_db(self, chat_id)
            .await
            .with_context(|| format!("Failed to load chat {chat_id} from the database"))?;
        if let Some(reason) = chat.why_cant_send(self).await.with_context(|| {
            format!("Failed to check if webxdc update can be sent to chat {chat_id}")
        })? {
            bail!("Cannot send to {chat_id}: {reason}.");
        }

        let send_now = !matches!(
            instance.state,
            MessageState::Undefined | MessageState::OutPreparing | MessageState::OutDraft
        );

        status_update.uid = Some(create_id());
        let status_update_serial: StatusUpdateSerial = self
            .create_status_update_record(
                &instance,
                status_update,
                create_smeared_timestamp(self),
                send_now,
                ContactId::SELF,
            )
            .await
            .context("Failed to create status update")?
            .context("Duplicate status update UID was generated")?;

        if send_now {
            self.sql.insert(
                "INSERT INTO smtp_status_updates (msg_id, first_serial, last_serial, descr) VALUES(?, ?, ?, ?)
                 ON CONFLICT(msg_id)
                 DO UPDATE SET last_serial=excluded.last_serial, descr=excluded.descr",
                (instance.id, status_update_serial, status_update_serial, descr),
            ).await.context("Failed to insert webxdc update into SMTP queue")?;
            self.scheduler.interrupt_smtp().await;
        }
        Ok(())
    }

    /// Pops one record of queued webxdc status updates.
    /// This function exists to make the sqlite statement testable.
    async fn pop_smtp_status_update(
        &self,
    ) -> Result<Option<(MsgId, StatusUpdateSerial, StatusUpdateSerial, String)>> {
        let _lock = self.sql.write_lock().await;
        let res = self
            .sql
            .query_row_optional(
                "DELETE FROM smtp_status_updates
                     WHERE msg_id IN (SELECT msg_id FROM smtp_status_updates LIMIT 1)
                     RETURNING msg_id, first_serial, last_serial, descr",
                (),
                |row| {
                    let instance_id: MsgId = row.get(0)?;
                    let first_serial: StatusUpdateSerial = row.get(1)?;
                    let last_serial: StatusUpdateSerial = row.get(2)?;
                    let descr: String = row.get(3)?;
                    Ok((instance_id, first_serial, last_serial, descr))
                },
            )
            .await?;
        Ok(res)
    }

    /// Attempts to send queued webxdc status updates.
    pub(crate) async fn flush_status_updates(&self) -> Result<()> {
        loop {
            let (instance_id, first_serial, last_serial, descr) =
                match self.pop_smtp_status_update().await? {
                    Some(res) => res,
                    None => return Ok(()),
                };

            if let Some(json) = self
                .render_webxdc_status_update_object(instance_id, Some((first_serial, last_serial)))
                .await?
            {
                let instance = Message::load_from_db(self, instance_id).await?;
                let mut status_update = Message {
                    chat_id: instance.chat_id,
                    viewtype: Viewtype::Text,
                    text: descr.to_string(),
                    hidden: true,
                    ..Default::default()
                };
                status_update
                    .param
                    .set_cmd(SystemMessage::WebxdcStatusUpdate);
                status_update.param.set(Param::Arg, json);
                status_update.set_quote(self, Some(&instance)).await?;
                status_update.param.remove(Param::GuaranteeE2ee); // may be set by set_quote(), if #2985 is done, this line can be removed
                chat::send_msg(self, instance.chat_id, &mut status_update).await?;
            }
        }
    }

    pub(crate) fn build_status_update_part(&self, json: &str) -> PartBuilder {
        let encoded_body = wrapped_base64_encode(json.as_bytes());

        PartBuilder::new()
            .content_type(&"application/json".parse::<mime::Mime>().unwrap())
            .header((
                "Content-Disposition",
                "attachment; filename=\"status-update.json\"",
            ))
            .header(("Content-Transfer-Encoding", "base64"))
            .body(encoded_body)
    }

    /// Receives status updates from receive_imf to the database
    /// and sends out an event.
    ///
    /// `instance` is a webxdc instance.
    ///
    /// `from_id` is the sender.
    ///
    /// `timestamp` is the timestamp of the update.
    ///
    /// `json` is an array containing one or more update items as created by send_webxdc_status_update(),
    /// the array is parsed using serde, the single payloads are used as is.
    pub(crate) async fn receive_status_update(
        &self,
        from_id: ContactId,
        instance: &Message,
        timestamp: i64,
        can_info_msg: bool,
        json: &str,
    ) -> Result<()> {
        let chat_id = instance.chat_id;

        if from_id != ContactId::SELF && !chat::is_contact_in_chat(self, chat_id, from_id).await? {
            let chat_type: Chattype = self
                .sql
                .query_get_value("SELECT type FROM chats WHERE id=?", (chat_id,))
                .await?
                .with_context(|| format!("Chat type for chat {chat_id} not found"))?;
            if chat_type != Chattype::Mailinglist {
                bail!("receive_status_update: status sender {from_id} is not a member of chat {chat_id}")
            }
        }

        let updates: StatusUpdates = serde_json::from_str(json)?;
        for update_item in updates.updates {
            self.create_status_update_record(
                instance,
                update_item,
                timestamp,
                can_info_msg,
                from_id,
            )
            .await?;
        }

        Ok(())
    }

    /// Returns status updates as an JSON-array, ready to be consumed by a webxdc.
    ///
    /// Example: `[{"serial":1, "max_serial":3, "payload":"any update data"},
    ///            {"serial":3, "max_serial":3, "payload":"another update data"}]`
    /// Updates with serials larger than `last_known_serial` are returned.
    /// If no last serial is known, set `last_known_serial` to 0.
    /// If no updates are available, an empty JSON-array is returned.
    pub async fn get_webxdc_status_updates(
        &self,
        instance_msg_id: MsgId,
        last_known_serial: StatusUpdateSerial,
    ) -> Result<String> {
        let param = instance_msg_id.get_param(self).await?;
        if param.get_int(Param::WebxdcIntegration).is_some() {
            let instance = Message::load_from_db(self, instance_msg_id).await?;
            return self
                .intercept_get_webxdc_status_updates(instance, last_known_serial)
                .await;
        }

        let json = self
            .sql
            .query_map(
                "SELECT update_item, id FROM msgs_status_updates WHERE msg_id=? AND id>? ORDER BY id",
                (instance_msg_id, last_known_serial),
                |row| {
                    let update_item_str = row.get::<_, String>(0)?;
                    let serial = row.get::<_, StatusUpdateSerial>(1)?;
                    Ok((update_item_str, serial))
                },
                |rows| {
                    let mut rows_copy : Vec<(String, StatusUpdateSerial)> = Vec::new(); // `rows_copy` needed as `rows` cannot be iterated twice.
                    let mut max_serial = StatusUpdateSerial(0);
                    for row in rows {
                        let row = row?;
                        if row.1 > max_serial {
                            max_serial = row.1;
                        }
                        rows_copy.push(row);
                    }

                    let mut json = String::default();
                    for row in rows_copy {
                        let (update_item_str, serial) = row;
                        let update_item = StatusUpdateItemAndSerial
                        {
                            item: StatusUpdateItem {
                                uid: None, // Erase UIDs, apps, bots and tests don't need to know them.
                                ..serde_json::from_str(&update_item_str)?
                            },
                            serial,
                            max_serial,
                        };

                        if !json.is_empty() {
                            json.push_str(",\n");
                        }
                        json.push_str(&serde_json::to_string(&update_item)?);
                    }
                    Ok(json)
                },
            )
            .await?;
        Ok(format!("[{json}]"))
    }

    /// Renders JSON-object for status updates as used on the wire.
    ///
    /// Example: `{"updates": [{"payload":"any update data"},
    ///                        {"payload":"another update data"}]}`
    ///
    /// `range` is an optional range of status update serials to send.
    /// If it is `None`, all updates are sent.
    /// This is used when a message is resent using [`crate::chat::resend_msgs`].
    pub(crate) async fn render_webxdc_status_update_object(
        &self,
        instance_msg_id: MsgId,
        range: Option<(StatusUpdateSerial, StatusUpdateSerial)>,
    ) -> Result<Option<String>> {
        let json = self
            .sql
            .query_map(
                "SELECT update_item FROM msgs_status_updates WHERE msg_id=? AND id>=? AND id<=? ORDER BY id",
                (
                    instance_msg_id,
                    range.map(|r|r.0).unwrap_or(StatusUpdateSerial(0)),
                    range.map(|r|r.1).unwrap_or(StatusUpdateSerial(u32::MAX)),
                ),
                |row| row.get::<_, String>(0),
                |rows| {
                    let mut json = String::default();
                    for row in rows {
                        let update_item = row?;
                        if !json.is_empty() {
                            json.push_str(",\n");
                        }
                        json.push_str(&update_item);
                    }
                    Ok(json)
                },
            )
            .await?;
        if json.is_empty() {
            Ok(None)
        } else {
            Ok(Some(format!(r#"{{"updates":[{json}]}}"#)))
        }
    }
}

fn parse_webxdc_manifest(bytes: &[u8]) -> Result<WebxdcManifest> {
    let s = std::str::from_utf8(bytes)?;
    let manifest: WebxdcManifest = toml::from_str(s)?;
    Ok(manifest)
}

async fn get_blob(archive: &async_zip::read::fs::ZipFileReader, name: &str) -> Result<Vec<u8>> {
    let (i, _) = find_zip_entry(archive.file(), name)
        .ok_or_else(|| anyhow!("no entry found for {}", name))?;
    let mut reader = archive.entry(i).await?;
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await?;
    Ok(buf)
}

impl Message {
    /// Get handle to a webxdc ZIP-archive.
    /// To check for file existence use archive.by_name(), to read a file, use get_blob(archive).
    async fn get_webxdc_archive(
        &self,
        context: &Context,
    ) -> Result<async_zip::read::fs::ZipFileReader> {
        let path = self
            .get_file(context)
            .ok_or_else(|| format_err!("No webxdc instance file."))?;
        let path_abs = get_abs_path(context, &path);
        let archive = async_zip::read::fs::ZipFileReader::new(path_abs).await?;
        Ok(archive)
    }

    /// Return file from inside an archive.
    /// Currently, this works only if the message is an webxdc instance.
    ///
    /// `name` is the filename within the archive, e.g. `index.html`.
    pub async fn get_webxdc_blob(&self, context: &Context, name: &str) -> Result<Vec<u8>> {
        ensure!(self.viewtype == Viewtype::Webxdc, "No webxdc instance.");

        if name == WEBXDC_DEFAULT_ICON {
            return Ok(include_bytes!("../assets/icon-webxdc.png").to_vec());
        }

        // ignore first slash.
        // this way, files can be accessed absolutely (`/index.html`) as well as relatively (`index.html`)
        let name = if name.starts_with('/') {
            name.split_at(1).1
        } else {
            name
        };

        let archive = self.get_webxdc_archive(context).await?;

        if name == "index.html" {
            if let Ok(bytes) = get_blob(&archive, "manifest.toml").await {
                if let Ok(manifest) = parse_webxdc_manifest(&bytes) {
                    if let Some(min_api) = manifest.min_api {
                        if min_api > WEBXDC_API_VERSION {
                            return Ok(Vec::from(
                                "<!DOCTYPE html>This Webxdc requires a newer Delta Chat version.",
                            ));
                        }
                    }
                }
            }
        }

        get_blob(&archive, name).await
    }

    /// Return info from manifest.toml or from fallbacks.
    pub async fn get_webxdc_info(&self, context: &Context) -> Result<WebxdcInfo> {
        ensure!(self.viewtype == Viewtype::Webxdc, "No webxdc instance.");
        let archive = self.get_webxdc_archive(context).await?;

        let mut manifest = get_blob(&archive, "manifest.toml")
            .await
            .map(|bytes| parse_webxdc_manifest(&bytes).unwrap_or_default())
            .unwrap_or_default();

        if let Some(ref name) = manifest.name {
            let name = name.trim();
            if name.is_empty() {
                warn!(context, "empty name given in manifest");
                manifest.name = None;
            }
        }

        let request_integration = manifest.request_integration.unwrap_or_default();
        let is_integrated = self.is_set_as_webxdc_integration(context).await?;

        let internet_access = manifest.request_internet_access.unwrap_or_default()
            && self.chat_id.is_self_talk(context).await.unwrap_or_default()
            && self.get_showpadlock();

        Ok(WebxdcInfo {
            name: if let Some(name) = manifest.name {
                name
            } else {
                self.get_filename().unwrap_or_default()
            },
            icon: if find_zip_entry(archive.file(), "icon.png").is_some() {
                "icon.png".to_string()
            } else if find_zip_entry(archive.file(), "icon.jpg").is_some() {
                "icon.jpg".to_string()
            } else {
                WEBXDC_DEFAULT_ICON.to_string()
            },
            document: self
                .param
                .get(Param::WebxdcDocument)
                .unwrap_or_default()
                .to_string(),
            summary: if is_integrated {
                "🌍 Used as map. Delete to use default. Do not enter sensitive data".to_string()
            } else if request_integration == "map" {
                "🌏 To use as map, forward to \"Saved Messages\" again. Do not enter sensitive data"
                    .to_string()
            } else if internet_access {
                "Dev Mode: Do not enter sensitive data!".to_string()
            } else {
                self.param
                    .get(Param::WebxdcSummary)
                    .unwrap_or_default()
                    .to_string()
            },
            source_code_url: if let Some(url) = manifest.source_code_url {
                url
            } else {
                "".to_string()
            },
            request_integration,
            internet_access,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use regex::Regex;
    use serde_json::json;

    use super::*;
    use crate::chat::{
        add_contact_to_chat, create_broadcast_list, create_group_chat, forward_msgs,
        remove_contact_from_chat, resend_msgs, send_msg, send_text_msg, ChatId, ProtectionStatus,
    };
    use crate::chatlist::Chatlist;
    use crate::config::Config;
    use crate::contact::Contact;
    use crate::download::DownloadState;
    use crate::ephemeral;
    use crate::receive_imf::{receive_imf, receive_imf_from_inbox};
    use crate::test_utils::{TestContext, TestContextManager};
    use crate::tools::{self, SystemTime};
    use crate::{message, sql};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_is_webxdc_file() -> Result<()> {
        let t = TestContext::new().await;
        assert!(
            !t.is_webxdc_file(
                "bad-ext-no-zip.txt",
                include_bytes!("../test-data/message/issue_523.txt")
            )
            .await?
        );
        assert!(
            !t.is_webxdc_file(
                "bad-ext-good-zip.txt",
                include_bytes!("../test-data/webxdc/minimal.xdc")
            )
            .await?
        );
        assert!(
            !t.is_webxdc_file(
                "good-ext-no-zip.xdc",
                include_bytes!("../test-data/message/issue_523.txt")
            )
            .await?
        );
        assert!(
            !t.is_webxdc_file(
                "good-ext-no-index-html.xdc",
                include_bytes!("../test-data/webxdc/no-index-html.xdc")
            )
            .await?
        );
        assert!(
            t.is_webxdc_file(
                "good-ext-good-zip.xdc",
                include_bytes!("../test-data/webxdc/minimal.xdc")
            )
            .await?
        );
        Ok(())
    }

    async fn create_webxdc_instance(t: &TestContext, name: &str, bytes: &[u8]) -> Result<Message> {
        let mut instance = Message::new(Viewtype::File);
        instance.set_file_from_bytes(t, name, bytes, None).await?;
        Ok(instance)
    }

    async fn send_webxdc_instance(t: &TestContext, chat_id: ChatId) -> Result<Message> {
        let mut instance = create_webxdc_instance(
            t,
            "minimal.xdc",
            include_bytes!("../test-data/webxdc/minimal.xdc"),
        )
        .await?;
        let instance_msg_id = send_msg(t, chat_id, &mut instance).await?;
        assert_eq!(instance.viewtype, Viewtype::Webxdc);
        Message::load_from_db(t, instance_msg_id).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_send_webxdc_instance() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;

        // send as .xdc file
        let instance = send_webxdc_instance(&t, chat_id).await?;
        assert_eq!(instance.viewtype, Viewtype::Webxdc);
        assert_eq!(instance.get_filename(), Some("minimal.xdc".to_string()));
        assert_eq!(instance.chat_id, chat_id);

        // sending using bad extension is not working, even when setting Viewtype to webxdc
        let mut instance = Message::new(Viewtype::Webxdc);
        instance
            .set_file_from_bytes(&t, "index.html", b"<html>ola!</html>", None)
            .await?;
        assert!(send_msg(&t, chat_id, &mut instance).await.is_err());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_send_invalid_webxdc() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;

        // sending invalid .xdc as file is possible, but must not result in Viewtype::Webxdc
        let mut instance = create_webxdc_instance(
            &t,
            "invalid-no-zip-but-7z.xdc",
            include_bytes!("../test-data/webxdc/invalid-no-zip-but-7z.xdc"),
        )
        .await?;
        let instance_id = send_msg(&t, chat_id, &mut instance).await?;
        assert_eq!(instance.viewtype, Viewtype::File);
        let test = Message::load_from_db(&t, instance_id).await?;
        assert_eq!(test.viewtype, Viewtype::File);

        // sending invalid .xdc as Viewtype::Webxdc should fail already on sending
        let mut instance = Message::new(Viewtype::Webxdc);
        instance
            .set_file_from_bytes(
                &t,
                "invalid2.xdc",
                include_bytes!("../test-data/webxdc/invalid-no-zip-but-7z.xdc"),
                None,
            )
            .await?;
        assert!(send_msg(&t, chat_id, &mut instance).await.is_err());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_send_special_webxdc_format() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;

        // chess.xdc is failing for some zip-versions, see #3476, if we know more details about why, we can have a nicer name for the test :)
        let mut instance = create_webxdc_instance(
            &t,
            "chess.xdc",
            include_bytes!("../test-data/webxdc/chess.xdc"),
        )
        .await?;
        let instance_id = send_msg(&t, chat_id, &mut instance).await?;
        let instance = Message::load_from_db(&t, instance_id).await?;
        assert_eq!(instance.viewtype, Viewtype::Webxdc);

        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "Chess Board");

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_forward_webxdc_instance() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;
        t.send_webxdc_status_update(
            instance.id,
            r#"{"info": "foo", "summary":"bar", "document":"doc", "payload": 42}"#,
            "descr",
        )
        .await?;
        assert!(!instance.is_forwarded());
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":42,"info":"foo","document":"doc","summary":"bar","serial":1,"max_serial":1}]"#
        );
        assert_eq!(chat_id.get_msg_cnt(&t).await?, 2); // instance and info
        let info = Message::load_from_db(&t, instance.id)
            .await?
            .get_webxdc_info(&t)
            .await?;
        assert_eq!(info.summary, "bar".to_string());
        assert_eq!(info.document, "doc".to_string());

        // forwarding an instance creates a fresh instance; updates etc. are not forwarded
        forward_msgs(&t, &[instance.get_id()], chat_id).await?;
        let instance2 = t.get_last_msg_in(chat_id).await;
        assert!(instance2.is_forwarded());
        assert_eq!(
            t.get_webxdc_status_updates(instance2.id, StatusUpdateSerial(0))
                .await?,
            "[]"
        );
        assert_eq!(chat_id.get_msg_cnt(&t).await?, 3); // two instances, only one info
        let info = Message::load_from_db(&t, instance2.id)
            .await?
            .get_webxdc_info(&t)
            .await?;
        assert_eq!(info.summary, "".to_string());
        assert_eq!(info.document, "".to_string());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_resend_webxdc_instance_and_info() -> Result<()> {
        let mut tcm = TestContextManager::new();

        // Alice uses webxdc in a group
        let alice = tcm.alice().await;
        alice.set_config_bool(Config::BccSelf, false).await?;
        let alice_grp = create_group_chat(&alice, ProtectionStatus::Unprotected, "grp").await?;
        let alice_instance = send_webxdc_instance(&alice, alice_grp).await?;
        assert_eq!(alice_grp.get_msg_cnt(&alice).await?, 1);
        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"payload":7,"info": "i","summary":"s"}"#,
                "d",
            )
            .await?;
        assert_eq!(alice_grp.get_msg_cnt(&alice).await?, 2);
        assert!(alice.get_last_msg_in(alice_grp).await.is_info());

        // Alice adds Bob and resends already used webxdc
        add_contact_to_chat(
            &alice,
            alice_grp,
            Contact::create(&alice, "", "bob@example.net").await?,
        )
        .await?;
        assert_eq!(alice_grp.get_msg_cnt(&alice).await?, 3);
        resend_msgs(&alice, &[alice_instance.id]).await?;
        let sent1 = alice.pop_sent_msg().await;

        // Bob received webxdc, legacy info-messages updates are received but not added to the chat
        let bob = tcm.bob().await;
        let bob_instance = bob.recv_msg(&sent1).await;
        assert_eq!(bob_instance.viewtype, Viewtype::Webxdc);
        assert!(!bob_instance.is_info());
        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":7,"info":"i","summary":"s","serial":1,"max_serial":1}]"#
        );
        let bob_grp = bob_instance.chat_id;
        assert_eq!(bob.get_last_msg_in(bob_grp).await.id, bob_instance.id);
        assert_eq!(bob_grp.get_msg_cnt(&bob).await?, 1);

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_receive_webxdc_instance() -> Result<()> {
        let t = TestContext::new_alice().await;
        receive_imf(
            &t,
            include_bytes!("../test-data/message/webxdc_good_extension.eml"),
            false,
        )
        .await?;
        let instance = t.get_last_msg().await;
        assert_eq!(instance.viewtype, Viewtype::Webxdc);
        assert_eq!(instance.get_filename().unwrap(), "minimal.xdc");

        receive_imf(
            &t,
            include_bytes!("../test-data/message/webxdc_bad_extension.eml"),
            false,
        )
        .await?;
        let instance = t.get_last_msg().await;
        assert_eq!(instance.viewtype, Viewtype::File); // we require the correct extension, only a mime type is not sufficient
        assert_eq!(instance.get_filename().unwrap(), "index.html");

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_contact_request() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;

        // Alice sends an webxdc instance to Bob
        let alice_chat = alice.create_chat(&bob).await;
        let _alice_instance = send_webxdc_instance(&alice, alice_chat.id).await?;
        bob.recv_msg(&alice.pop_sent_msg().await).await;

        // Bob can start the webxdc from a contact request (get index.html)
        // but cannot send updates to contact requests
        let bob_instance = bob.get_last_msg().await;
        let bob_chat = Chat::load_from_db(&bob, bob_instance.chat_id).await?;
        assert!(bob_chat.is_contact_request());
        assert!(bob_instance
            .get_webxdc_blob(&bob, "index.html")
            .await
            .is_ok());
        assert!(bob
            .send_webxdc_status_update(bob_instance.id, r#"{"payload":42}"#, "descr")
            .await
            .is_err());
        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            "[]"
        );

        // Once the contact request is accepted, Bob can send updates
        bob_chat.id.accept(&bob).await?;
        assert!(bob
            .send_webxdc_status_update(bob_instance.id, r#"{"payload":42}"#, "descr")
            .await
            .is_ok());
        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":42,"serial":1,"max_serial":1}]"#
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_update_for_not_downloaded_instance() -> Result<()> {
        // Alice sends a larger instance and an update
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;
        let chat = alice.create_chat(&bob).await;
        bob.set_config(Config::DownloadLimit, Some("40000")).await?;
        let mut alice_instance = create_webxdc_instance(
            &alice,
            "chess.xdc",
            include_bytes!("../test-data/webxdc/chess.xdc"),
        )
        .await?;
        let sent1 = alice.send_msg(chat.id, &mut alice_instance).await;
        let alice_instance = sent1.load_from_db().await;
        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"payload": 7, "summary":"sum", "document":"doc"}"#,
                "bla",
            )
            .await?;
        alice.flush_status_updates().await?;
        let sent2 = alice.pop_sent_msg().await;

        // Bob does not download instance but already receives update
        receive_imf_from_inbox(
            &bob,
            &alice_instance.rfc724_mid,
            sent1.payload().as_bytes(),
            false,
            Some(70790),
            false,
        )
        .await?;
        let bob_instance = bob.get_last_msg().await;
        bob_instance.chat_id.accept(&bob).await?;
        bob.recv_msg_trash(&sent2).await;
        assert_eq!(bob_instance.download_state, DownloadState::Available);

        // Bob downloads instance, updates should be assigned correctly
        let received_msg = receive_imf_from_inbox(
            &bob,
            &alice_instance.rfc724_mid,
            sent1.payload().as_bytes(),
            false,
            None,
            false,
        )
        .await?
        .unwrap();
        assert_eq!(*received_msg.msg_ids.first().unwrap(), bob_instance.id);
        let bob_instance = bob.get_last_msg().await;
        assert_eq!(bob_instance.viewtype, Viewtype::Webxdc);
        assert_eq!(bob_instance.download_state, DownloadState::Done);
        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":7,"document":"doc","summary":"sum","serial":1,"max_serial":1}]"#
        );
        let info = bob_instance.get_webxdc_info(&bob).await?;
        assert_eq!(info.document, "doc");
        assert_eq!(info.summary, "sum");

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_delete_webxdc_instance() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;
        let now = tools::time();
        t.receive_status_update(
            ContactId::SELF,
            &instance,
            now,
            true,
            r#"{"updates":[{"payload":1}]}"#,
        )
        .await?;
        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM msgs_status_updates;", ())
                .await?,
            1
        );

        message::delete_msgs(&t, &[instance.id]).await?;
        sql::housekeeping(&t).await?;
        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM msgs_status_updates;", ())
                .await?,
            0
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_delete_chat_with_webxdc() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;
        let now = tools::time();
        t.receive_status_update(
            ContactId::SELF,
            &instance,
            now,
            true,
            r#"{"updates":[{"payload":1}, {"payload":2}]}"#,
        )
        .await?;
        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM msgs_status_updates;", ())
                .await?,
            2
        );

        chat_id.delete(&t).await?;
        sql::housekeeping(&t).await?;
        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM msgs_status_updates;", ())
                .await?,
            0
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_delete_webxdc_draft() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;

        let mut instance = create_webxdc_instance(
            &t,
            "minimal.xdc",
            include_bytes!("../test-data/webxdc/minimal.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        let instance = chat_id.get_draft(&t).await?.unwrap();
        t.send_webxdc_status_update(instance.id, r#"{"payload": 42}"#, "descr")
            .await?;
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":42,"serial":1,"max_serial":1}]"#.to_string()
        );

        // set_draft(None) deletes the message without the need to simulate network
        chat_id.set_draft(&t, None).await?;
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            "[]".to_string()
        );
        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM msgs_status_updates;", ())
                .await?,
            0
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_create_status_update_record() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;

        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            "[]"
        );

        let update_id1 = t
            .create_status_update_record(
                &instance,
                StatusUpdateItem {
                    payload: json!({"foo": "bar"}),
                    info: None,
                    document: None,
                    summary: None,
                    uid: Some("iecie2Ze".to_string()),
                },
                1640178619,
                true,
                ContactId::SELF,
            )
            .await?
            .unwrap();
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":1}]"#
        );

        // Update with duplicate update ID is received.
        // Whatever the payload is, update should be ignored just because ID is duplicate.
        let update_id1_duplicate = t
            .create_status_update_record(
                &instance,
                StatusUpdateItem {
                    payload: json!({"nothing": "this should be ignored"}),
                    info: None,
                    document: None,
                    summary: None,
                    uid: Some("iecie2Ze".to_string()),
                },
                1640178619,
                true,
                ContactId::SELF,
            )
            .await?;
        assert_eq!(update_id1_duplicate, None);

        assert!(t
            .send_webxdc_status_update(instance.id, "\n\n\n", "")
            .await
            .is_err());

        assert!(t
            .send_webxdc_status_update(instance.id, "bad json", "")
            .await
            .is_err());

        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":1}]"#
        );

        let update_id2 = t
            .create_status_update_record(
                &instance,
                StatusUpdateItem {
                    payload: json!({"foo2": "bar2"}),
                    info: None,
                    document: None,
                    summary: None,
                    uid: None,
                },
                1640178619,
                true,
                ContactId::SELF,
            )
            .await?
            .unwrap();
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, update_id1).await?,
            r#"[{"payload":{"foo2":"bar2"},"serial":3,"max_serial":3}]"#
        );
        t.create_status_update_record(
            &instance,
            StatusUpdateItem {
                payload: Value::Bool(true),
                info: None,
                document: None,
                summary: None,
                uid: None,
            },
            1640178619,
            true,
            ContactId::SELF,
        )
        .await?;
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":4},
{"payload":{"foo2":"bar2"},"serial":3,"max_serial":4},
{"payload":true,"serial":4,"max_serial":4}]"#
        );

        t.send_webxdc_status_update(
            instance.id,
            r#"{"payload" : 1, "sender": "that is not used"}"#,
            "",
        )
        .await?;
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, update_id2).await?,
            r#"[{"payload":true,"serial":4,"max_serial":5},
{"payload":1,"serial":5,"max_serial":5}]"#
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_receive_status_update() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;
        let now = tools::time();

        assert!(t
            .receive_status_update(ContactId::SELF, &instance, now, true, r#"foo: bar"#)
            .await
            .is_err()); // no json
        assert!(t
            .receive_status_update(
                ContactId::SELF,
                &instance,
                now,
                true,
                r#"{"updada":[{"payload":{"foo":"bar"}}]}"#
            )
            .await
            .is_err()); // "updates" object missing
        assert!(t
            .receive_status_update(
                ContactId::SELF,
                &instance,
                now,
                true,
                r#"{"updates":[{"foo":"bar"}]}"#
            )
            .await
            .is_err()); // "payload" field missing
        assert!(t
            .receive_status_update(
                ContactId::SELF,
                &instance,
                now,
                true,
                r#"{"updates":{"payload":{"foo":"bar"}}}"#
            )
            .await
            .is_err()); // not an array

        t.receive_status_update(
            ContactId::SELF,
            &instance,
            now,
            true,
            r#"{"updates":[{"payload":{"foo":"bar"}, "someTrash": "definitely TrAsH"}]}"#,
        )
        .await?;
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":1}]"#
        );

        t.receive_status_update(
            ContactId::SELF,
            &instance,
            now,
            true,
            r#" {"updates": [ {"payload" :42} , {"payload": 23} ] } "#,
        )
        .await?;
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":3},
{"payload":42,"serial":2,"max_serial":3},
{"payload":23,"serial":3,"max_serial":3}]"#
        );

        t.receive_status_update(
            ContactId::SELF,
            &instance,
            now,
            true,
            r#" {"updates": [ {"payload" :"ok", "future_item": "test"}  ], "from": "future" } "#,
        )
        .await?; // ignore members that may be added in the future
        assert_eq!(
            t.get_webxdc_status_updates(instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":4},
{"payload":42,"serial":2,"max_serial":4},
{"payload":23,"serial":3,"max_serial":4},
{"payload":"ok","serial":4,"max_serial":4}]"#
        );

        Ok(())
    }

    async fn expect_status_update_event(t: &TestContext, instance_id: MsgId) -> Result<()> {
        let event = t
            .evtracker
            .get_matching(|evt| matches!(evt, EventType::WebxdcStatusUpdate { .. }))
            .await;
        match event {
            EventType::WebxdcStatusUpdate {
                msg_id,
                status_update_serial: _,
            } => {
                assert_eq!(msg_id, instance_id);
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_send_webxdc_status_update() -> Result<()> {
        let alice = TestContext::new_alice().await;
        alice.set_config_bool(Config::BccSelf, true).await?;
        let bob = TestContext::new_bob().await;

        // Alice sends an webxdc instance and a status update
        let alice_chat = alice.create_chat(&bob).await;
        let alice_instance = send_webxdc_instance(&alice, alice_chat.id).await?;
        let sent1 = &alice.pop_sent_msg().await;
        assert_eq!(alice_instance.viewtype, Viewtype::Webxdc);
        assert!(!sent1.payload().contains("report-type=status-update"));

        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"payload" : {"foo":"bar"}}"#,
                "descr text",
            )
            .await?;
        alice.flush_status_updates().await?;
        expect_status_update_event(&alice, alice_instance.id).await?;
        let sent2 = &alice.pop_sent_msg().await;
        let alice_update = sent2.load_from_db().await;
        assert!(alice_update.hidden);
        assert_eq!(alice_update.viewtype, Viewtype::Text);
        assert_eq!(alice_update.get_filename(), None);
        assert_eq!(alice_update.text, "descr text".to_string());
        assert_eq!(alice_update.chat_id, alice_instance.chat_id);
        assert_eq!(
            alice_update.parent(&alice).await?.unwrap().id,
            alice_instance.id
        );
        assert_eq!(alice_chat.id.get_msg_cnt(&alice).await?, 1);
        assert!(sent2.payload().contains("report-type=status-update"));
        assert!(sent2.payload().contains("descr text"));
        assert_eq!(
            alice
                .get_webxdc_status_updates(alice_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":1}]"#
        );

        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"payload":{"snipp":"snapp"}}"#,
                "bla text",
            )
            .await?;
        assert_eq!(
            alice
                .get_webxdc_status_updates(alice_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":2},
{"payload":{"snipp":"snapp"},"serial":2,"max_serial":2}]"#
        );

        // Bob receives all messages
        let bob_instance = bob.recv_msg(sent1).await;
        let bob_chat_id = bob_instance.chat_id;
        assert_eq!(bob_instance.rfc724_mid, alice_instance.rfc724_mid);
        assert_eq!(bob_instance.viewtype, Viewtype::Webxdc);
        assert_eq!(bob_chat_id.get_msg_cnt(&bob).await?, 1);

        let bob_received_update = bob.recv_msg_opt(sent2).await;
        assert!(bob_received_update.is_none());
        expect_status_update_event(&bob, bob_instance.id).await?;
        assert_eq!(bob_chat_id.get_msg_cnt(&bob).await?, 1);

        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":1}]"#
        );

        // Alice has a second device and also receives messages there
        let alice2 = TestContext::new_alice().await;
        alice2.recv_msg(sent1).await;
        alice2.recv_msg_trash(sent2).await;
        let alice2_instance = alice2.get_last_msg().await;
        let alice2_chat_id = alice2_instance.chat_id;
        assert_eq!(alice2_instance.viewtype, Viewtype::Webxdc);
        assert_eq!(alice2_chat_id.get_msg_cnt(&alice2).await?, 1);

        // To support the second device, Alice has enabled bcc_self and will receive their own messages;
        // these messages, however, should be ignored
        alice.recv_msg_opt(sent1).await;
        alice.recv_msg_opt(sent2).await;
        assert_eq!(alice_chat.id.get_msg_cnt(&alice).await?, 1);
        assert_eq!(
            alice
                .get_webxdc_status_updates(alice_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":2},
{"payload":{"snipp":"snapp"},"serial":2,"max_serial":2}]"#
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_render_webxdc_status_update_object() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "a chat").await?;
        let mut instance = create_webxdc_instance(
            &t,
            "minimal.xdc",
            include_bytes!("../test-data/webxdc/minimal.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        assert!(t
            .render_webxdc_status_update_object(instance.id, None)
            .await?
            .is_none());

        t.send_webxdc_status_update(instance.id, r#"{"payload": 1}"#, "bla")
            .await?;
        assert!(t
            .render_webxdc_status_update_object(instance.id, None)
            .await?
            .is_some());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_render_webxdc_status_update_object_range() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "a chat").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;
        t.send_webxdc_status_update(instance.id, r#"{"payload": 1}"#, "d")
            .await?;
        t.send_webxdc_status_update(instance.id, r#"{"payload": 2}"#, "d")
            .await?;
        t.send_webxdc_status_update(instance.id, r#"{"payload": 3}"#, "d")
            .await?;
        t.send_webxdc_status_update(instance.id, r#"{"payload": 4}"#, "d")
            .await?;
        let json = t
            .render_webxdc_status_update_object(
                instance.id,
                Some((StatusUpdateSerial(2), StatusUpdateSerial(3))),
            )
            .await?
            .unwrap();
        let json = Regex::new(r#""uid":"[^"]*""#)
            .unwrap()
            .replace_all(&json, "XXX");
        assert_eq!(
            json,
            "{\"updates\":[{\"payload\":2,XXX},\n{\"payload\":3,XXX}]}"
        );

        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM smtp_status_updates", ())
                .await?,
            1
        );
        t.flush_status_updates().await?;
        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM smtp_status_updates", ())
                .await?,
            0
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_pop_status_update() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "a chat").await?;
        let instance1 = send_webxdc_instance(&t, chat_id).await?;
        let instance2 = send_webxdc_instance(&t, chat_id).await?;
        let instance3 = send_webxdc_instance(&t, chat_id).await?;
        assert!(t.pop_smtp_status_update().await?.is_none());

        t.send_webxdc_status_update(instance1.id, r#"{"payload": "1a"}"#, "descr1a")
            .await?;
        t.send_webxdc_status_update(instance2.id, r#"{"payload": "2a"}"#, "descr2a")
            .await?;
        t.send_webxdc_status_update(instance2.id, r#"{"payload": "2b"}"#, "descr2b")
            .await?;
        t.send_webxdc_status_update(instance3.id, r#"{"payload": "3a"}"#, "descr3a")
            .await?;
        t.send_webxdc_status_update(instance3.id, r#"{"payload": "3b"}"#, "descr3b")
            .await?;
        t.send_webxdc_status_update(instance3.id, r#"{"payload": "3c"}"#, "descr3c")
            .await?;
        assert_eq!(
            t.sql
                .count("SELECT COUNT(*) FROM smtp_status_updates", ())
                .await?,
            3
        );

        // order of pop_status_update() is not defined, therefore the more complicated test
        let mut instances_checked = 0;
        for i in 0..3 {
            let (instance, min_ser, max_ser, descr) = t.pop_smtp_status_update().await?.unwrap();
            if instance == instance1.id {
                assert_eq!(min_ser, max_ser);
                assert_eq!(descr, "descr1a");
                instances_checked += 1;
            } else if instance == instance2.id {
                assert_eq!(min_ser.to_u32(), max_ser.to_u32() - 1);
                assert_eq!(descr, "descr2b");
                instances_checked += 1;
            } else if instance == instance3.id {
                assert_eq!(min_ser.to_u32(), max_ser.to_u32() - 2);
                assert_eq!(descr, "descr3c");
                instances_checked += 1;
            } else {
                bail!("unexpected instance");
            }
            assert_eq!(
                t.sql
                    .count("SELECT COUNT(*) FROM smtp_status_updates", ())
                    .await?,
                2 - i
            );
        }
        assert_eq!(instances_checked, 3);
        assert!(t.pop_smtp_status_update().await?.is_none());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_draft_and_send_webxdc_status_update() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;
        let alice_chat_id = alice.create_chat(&bob).await.id;

        // prepare webxdc instance,
        // status updates are not sent for drafts, therefore send_webxdc_status_update() returns Ok(None)
        let mut alice_instance = create_webxdc_instance(
            &alice,
            "minimal.xdc",
            include_bytes!("../test-data/webxdc/minimal.xdc"),
        )
        .await?;
        alice_chat_id
            .set_draft(&alice, Some(&mut alice_instance))
            .await?;
        let mut alice_instance = alice_chat_id.get_draft(&alice).await?.unwrap();

        alice
            .send_webxdc_status_update(alice_instance.id, r#"{"payload": {"foo":"bar"}}"#, "descr")
            .await?;
        alice.flush_status_updates().await?;
        expect_status_update_event(&alice, alice_instance.id).await?;
        alice
            .send_webxdc_status_update(alice_instance.id, r#"{"payload":42, "info":"i"}"#, "descr")
            .await?;
        alice.flush_status_updates().await?;
        assert_eq!(
            alice
                .sql
                .count("SELECT COUNT(*) FROM smtp_status_updates", ())
                .await?,
            0
        );
        assert!(!alice.get_last_msg().await.is_info()); // 'info: "i"' message not added in draft mode

        // send webxdc instance,
        // the initial status updates are sent together in the same message
        let alice_instance_id = send_msg(&alice, alice_chat_id, &mut alice_instance).await?;
        let sent1 = alice.pop_sent_msg().await;
        let alice_instance = Message::load_from_db(&alice, alice_instance_id).await?;
        assert_eq!(alice_instance.viewtype, Viewtype::Webxdc);
        assert_eq!(
            alice_instance.get_filename(),
            Some("minimal.xdc".to_string())
        );
        assert_eq!(alice_instance.chat_id, alice_chat_id);

        // bob receives the instance together with the initial updates in a single message
        let bob_instance = bob.recv_msg(&sent1).await;
        assert_eq!(bob_instance.viewtype, Viewtype::Webxdc);
        assert_eq!(bob_instance.get_filename().unwrap(), "minimal.xdc");
        assert!(sent1.payload().contains("Content-Type: application/json"));
        assert!(sent1.payload().contains("status-update.json"));
        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":2},
{"payload":42,"info":"i","serial":2,"max_serial":2}]"#
        );
        assert!(!bob.get_last_msg().await.is_info()); // 'info: "i"' message not added in draft mode

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_send_webxdc_status_update_to_non_webxdc() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let msg_id = send_text_msg(&t, chat_id, "ho!".to_string()).await?;
        assert!(t
            .send_webxdc_status_update(msg_id, r#"{"foo":"bar"}"#, "descr")
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_get_webxdc_blob() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;

        let buf = instance.get_webxdc_blob(&t, "index.html").await?;
        assert_eq!(buf.len(), 188);
        assert!(String::from_utf8_lossy(&buf).contains("document.write"));

        assert!(instance
            .get_webxdc_blob(&t, "not-existent.html")
            .await
            .is_err());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_get_webxdc_blob_default_icon() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;

        let buf = instance.get_webxdc_blob(&t, WEBXDC_DEFAULT_ICON).await?;
        assert!(buf.len() > 100);
        assert!(String::from_utf8_lossy(&buf).contains("PNG\r\n"));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_get_webxdc_blob_with_absolute_paths() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;

        let buf = instance.get_webxdc_blob(&t, "/index.html").await?;
        assert!(String::from_utf8_lossy(&buf).contains("document.write"));

        assert!(instance.get_webxdc_blob(&t, "/not-there").await.is_err());
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_get_webxdc_blob_with_subdirs() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;
        let mut instance = create_webxdc_instance(
            &t,
            "some-files.xdc",
            include_bytes!("../test-data/webxdc/some-files.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;

        let buf = instance.get_webxdc_blob(&t, "index.html").await?;
        assert_eq!(buf.len(), 65);
        assert!(String::from_utf8_lossy(&buf).contains("many files"));

        let buf = instance.get_webxdc_blob(&t, "subdir/bla.txt").await?;
        assert_eq!(buf.len(), 4);
        assert!(String::from_utf8_lossy(&buf).starts_with("bla"));

        let buf = instance
            .get_webxdc_blob(&t, "subdir/subsubdir/text.md")
            .await?;
        assert_eq!(buf.len(), 24);
        assert!(String::from_utf8_lossy(&buf).starts_with("this is a markdown file"));

        let buf = instance
            .get_webxdc_blob(&t, "subdir/subsubdir/text2.md")
            .await?;
        assert_eq!(buf.len(), 22);
        assert!(String::from_utf8_lossy(&buf).starts_with("another markdown"));

        let buf = instance
            .get_webxdc_blob(&t, "anotherdir/anothersubsubdir/foo.txt")
            .await?;
        assert_eq!(buf.len(), 4);
        assert!(String::from_utf8_lossy(&buf).starts_with("foo"));

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_parse_webxdc_manifest() -> Result<()> {
        let result = parse_webxdc_manifest(r#"key = syntax error"#.as_bytes());
        assert!(result.is_err());

        let manifest = parse_webxdc_manifest(r#"no_name = "no name, no icon""#.as_bytes())?;
        assert_eq!(manifest.name, None);

        let manifest = parse_webxdc_manifest(r#"name = "name, no icon""#.as_bytes())?;
        assert_eq!(manifest.name, Some("name, no icon".to_string()));

        let manifest = parse_webxdc_manifest(
            r#"name = "foo"
icon = "bar""#
                .as_bytes(),
        )?;
        assert_eq!(manifest.name, Some("foo".to_string()));

        let manifest = parse_webxdc_manifest(
            r#"name = "foz"
icon = "baz"
add_item = "that should be just ignored"

[section]
sth_for_the = "future""#
                .as_bytes(),
        )?;
        assert_eq!(manifest.name, Some("foz".to_string()));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_parse_webxdc_manifest_min_api() -> Result<()> {
        let manifest = parse_webxdc_manifest(r#"min_api = 3"#.as_bytes())?;
        assert_eq!(manifest.min_api, Some(3));

        let result = parse_webxdc_manifest(r#"min_api = "1""#.as_bytes());
        assert!(result.is_err());

        let result = parse_webxdc_manifest(r#"min_api = 1.2"#.as_bytes());
        assert!(result.is_err());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_parse_webxdc_manifest_source_code_url() -> Result<()> {
        let result = parse_webxdc_manifest(r#"source_code_url = 3"#.as_bytes());
        assert!(result.is_err());

        let manifest = parse_webxdc_manifest(r#"source_code_url = "https://foo.bar""#.as_bytes())?;
        assert_eq!(
            manifest.source_code_url,
            Some("https://foo.bar".to_string())
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_parse_webxdc_manifest_request_internet_access() -> Result<()> {
        let result = parse_webxdc_manifest(r#"request_internet_access = 3"#.as_bytes());
        assert!(result.is_err());
        let manifest = parse_webxdc_manifest(r#" source_code_url="https://foo.org""#.as_bytes())?;
        assert_eq!(manifest.request_internet_access, None);
        let manifest = parse_webxdc_manifest(r#" request_internet_access=false"#.as_bytes())?;
        assert_eq!(manifest.request_internet_access, Some(false));
        let manifest = parse_webxdc_manifest(r#"request_internet_access = true"#.as_bytes())?;
        assert_eq!(manifest.request_internet_access, Some(true));
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_min_api_too_large() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "chat").await?;
        let mut instance = create_webxdc_instance(
            &t,
            "with-min-api-1001.xdc",
            include_bytes!("../test-data/webxdc/with-min-api-1001.xdc"),
        )
        .await?;
        send_msg(&t, chat_id, &mut instance).await?;

        let instance = t.get_last_msg().await;
        let html = instance.get_webxdc_blob(&t, "index.html").await?;
        assert!(String::from_utf8_lossy(&html).contains("requires a newer Delta Chat version"));

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_get_webxdc_info() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "foo").await?;

        let instance = send_webxdc_instance(&t, chat_id).await?;
        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "minimal.xdc");
        assert_eq!(info.icon, WEBXDC_DEFAULT_ICON.to_string());

        let mut instance = create_webxdc_instance(
            &t,
            "with-manifest-empty-name.xdc",
            include_bytes!("../test-data/webxdc/with-manifest-empty-name.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "with-manifest-empty-name.xdc");
        assert_eq!(info.icon, WEBXDC_DEFAULT_ICON.to_string());

        let mut instance = create_webxdc_instance(
            &t,
            "with-manifest-no-name.xdc",
            include_bytes!("../test-data/webxdc/with-manifest-no-name.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "with-manifest-no-name.xdc");
        assert_eq!(info.icon, WEBXDC_DEFAULT_ICON.to_string());

        let mut instance = create_webxdc_instance(
            &t,
            "with-minimal-manifest.xdc",
            include_bytes!("../test-data/webxdc/with-minimal-manifest.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "nice app!");
        assert_eq!(info.icon, WEBXDC_DEFAULT_ICON.to_string());

        let mut instance = create_webxdc_instance(
            &t,
            "with-manifest-and-png-icon.xdc",
            include_bytes!("../test-data/webxdc/with-manifest-and-png-icon.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "with some icon");
        assert_eq!(info.icon, "icon.png");

        let mut instance = create_webxdc_instance(
            &t,
            "with-png-icon.xdc",
            include_bytes!("../test-data/webxdc/with-png-icon.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "with-png-icon.xdc");
        assert_eq!(info.icon, "icon.png");

        let mut instance = create_webxdc_instance(
            &t,
            "with-jpg-icon.xdc",
            include_bytes!("../test-data/webxdc/with-jpg-icon.xdc"),
        )
        .await?;
        chat_id.set_draft(&t, Some(&mut instance)).await?;
        let info = instance.get_webxdc_info(&t).await?;
        assert_eq!(info.name, "with-jpg-icon.xdc");
        assert_eq!(info.icon, "icon.jpg");

        let msg_id = send_text_msg(&t, chat_id, "foo".to_string()).await?;
        let msg = Message::load_from_db(&t, msg_id).await?;
        let result = msg.get_webxdc_info(&t).await;
        assert!(result.is_err());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_info_summary() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;

        // Alice creates an webxdc instance and updates summary
        let alice_chat = alice.create_chat(&bob).await;
        let alice_instance = send_webxdc_instance(&alice, alice_chat.id).await?;
        let sent_instance = &alice.pop_sent_msg().await;
        let info = alice_instance.get_webxdc_info(&alice).await?;
        assert_eq!(info.summary, "".to_string());

        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"summary":"sum: 1", "payload":1}"#,
                "descr",
            )
            .await?;
        alice.flush_status_updates().await?;
        let sent_update1 = &alice.pop_sent_msg().await;
        let info = Message::load_from_db(&alice, alice_instance.id)
            .await?
            .get_webxdc_info(&alice)
            .await?;
        assert_eq!(info.summary, "sum: 1".to_string());

        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"summary":"sum: 2", "payload":2}"#,
                "descr",
            )
            .await?;
        alice.flush_status_updates().await?;
        let sent_update2 = &alice.pop_sent_msg().await;
        let info = Message::load_from_db(&alice, alice_instance.id)
            .await?
            .get_webxdc_info(&alice)
            .await?;
        assert_eq!(info.summary, "sum: 2".to_string());

        // Bob receives the updates
        let bob_instance = bob.recv_msg(sent_instance).await;
        bob.recv_msg_trash(sent_update1).await;
        bob.recv_msg_trash(sent_update2).await;
        let info = Message::load_from_db(&bob, bob_instance.id)
            .await?
            .get_webxdc_info(&bob)
            .await?;
        assert_eq!(info.summary, "sum: 2".to_string());

        // Alice has a second device and also receives the updates there
        let alice2 = TestContext::new_alice().await;
        let alice2_instance = alice2.recv_msg(sent_instance).await;
        alice2.recv_msg_trash(sent_update1).await;
        alice2.recv_msg_trash(sent_update2).await;
        let info = Message::load_from_db(&alice2, alice2_instance.id)
            .await?
            .get_webxdc_info(&alice2)
            .await?;
        assert_eq!(info.summary, "sum: 2".to_string());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_document_name() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;

        // Alice creates an webxdc instance and updates document name
        let alice_chat = alice.create_chat(&bob).await;
        let alice_instance = send_webxdc_instance(&alice, alice_chat.id).await?;
        let sent_instance = &alice.pop_sent_msg().await;
        let info = alice_instance.get_webxdc_info(&alice).await?;
        assert_eq!(info.document, "".to_string());
        assert_eq!(info.summary, "".to_string());

        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"document":"my file", "payload":1337}"#,
                "descr",
            )
            .await?;
        alice.flush_status_updates().await?;
        let sent_update1 = &alice.pop_sent_msg().await;
        let info = Message::load_from_db(&alice, alice_instance.id)
            .await?
            .get_webxdc_info(&alice)
            .await?;
        assert_eq!(info.document, "my file".to_string());
        assert_eq!(info.summary, "".to_string());

        // Bob receives the updates
        let bob_instance = bob.recv_msg(sent_instance).await;
        bob.recv_msg_trash(sent_update1).await;
        let info = Message::load_from_db(&bob, bob_instance.id)
            .await?
            .get_webxdc_info(&bob)
            .await?;
        assert_eq!(info.document, "my file".to_string());
        assert_eq!(info.summary, "".to_string());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_info_msg() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;

        // Alice sends update with an info message
        let alice_chat = alice.create_chat(&bob).await;
        let alice_instance = send_webxdc_instance(&alice, alice_chat.id).await?;
        let sent1 = &alice.pop_sent_msg().await;
        assert_eq!(alice_chat.id.get_msg_cnt(&alice).await?, 1);

        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"info":"this appears in-chat", "payload":"sth. else"}"#,
                "descr text",
            )
            .await?;
        alice.flush_status_updates().await?;
        let sent2 = &alice.pop_sent_msg().await;
        assert_eq!(alice_chat.id.get_msg_cnt(&alice).await?, 2);
        let info_msg = alice.get_last_msg().await;
        assert!(info_msg.is_info());
        assert_eq!(info_msg.get_info_type(), SystemMessage::WebxdcInfoMessage);
        assert_eq!(info_msg.from_id, ContactId::SELF);
        assert_eq!(info_msg.get_text(), "this appears in-chat");
        assert_eq!(
            info_msg.parent(&alice).await?.unwrap().id,
            alice_instance.id
        );
        assert!(info_msg.quoted_message(&alice).await?.is_none());
        assert_eq!(
            alice
                .get_webxdc_status_updates(alice_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":"sth. else","info":"this appears in-chat","serial":1,"max_serial":1}]"#
        );

        // Bob receives all messages
        let bob_instance = bob.recv_msg(sent1).await;
        let bob_chat_id = bob_instance.chat_id;
        bob.recv_msg_trash(sent2).await;
        assert_eq!(bob_chat_id.get_msg_cnt(&bob).await?, 2);
        let info_msg = bob.get_last_msg().await;
        assert!(info_msg.is_info());
        assert_eq!(info_msg.get_info_type(), SystemMessage::WebxdcInfoMessage);
        assert!(!info_msg.from_id.is_special());
        assert_eq!(info_msg.get_text(), "this appears in-chat");
        assert_eq!(info_msg.parent(&bob).await?.unwrap().id, bob_instance.id);
        assert!(info_msg.quoted_message(&bob).await?.is_none());
        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":"sth. else","info":"this appears in-chat","serial":1,"max_serial":1}]"#
        );

        // Alice has a second device and also receives the info message there
        let alice2 = TestContext::new_alice().await;
        let alice2_instance = alice2.recv_msg(sent1).await;
        let alice2_chat_id = alice2_instance.chat_id;
        alice2.recv_msg_trash(sent2).await;
        assert_eq!(alice2_chat_id.get_msg_cnt(&alice2).await?, 2);
        let info_msg = alice2.get_last_msg().await;
        assert!(info_msg.is_info());
        assert_eq!(info_msg.get_info_type(), SystemMessage::WebxdcInfoMessage);
        assert_eq!(info_msg.from_id, ContactId::SELF);
        assert_eq!(info_msg.get_text(), "this appears in-chat");
        assert_eq!(
            info_msg.parent(&alice2).await?.unwrap().id,
            alice2_instance.id
        );
        assert!(info_msg.quoted_message(&alice2).await?.is_none());
        assert_eq!(
            alice2
                .get_webxdc_status_updates(alice2_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":"sth. else","info":"this appears in-chat","serial":1,"max_serial":1}]"#
        );

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_info_msg_cleanup_series() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;
        let alice_chat = alice.create_chat(&bob).await;
        let alice_instance = send_webxdc_instance(&alice, alice_chat.id).await?;
        let sent1 = &alice.pop_sent_msg().await;

        // Alice sends two info messages in a row;
        // the second one removes the first one as there is nothing in between
        alice
            .send_webxdc_status_update(alice_instance.id, r#"{"info":"i1", "payload":1}"#, "d")
            .await?;
        alice.flush_status_updates().await?;
        let sent2 = &alice.pop_sent_msg().await;
        assert_eq!(alice_chat.id.get_msg_cnt(&alice).await?, 2);
        alice
            .send_webxdc_status_update(alice_instance.id, r#"{"info":"i2", "payload":2}"#, "d")
            .await?;
        alice.flush_status_updates().await?;
        let sent3 = &alice.pop_sent_msg().await;
        assert_eq!(alice_chat.id.get_msg_cnt(&alice).await?, 2);
        let info_msg = alice.get_last_msg().await;
        assert_eq!(info_msg.get_text(), "i2");

        // When Bob receives the messages, they should be cleaned up as well
        let bob_instance = bob.recv_msg(sent1).await;
        let bob_chat_id = bob_instance.chat_id;
        bob.recv_msg_trash(sent2).await;
        assert_eq!(bob_chat_id.get_msg_cnt(&bob).await?, 2);
        bob.recv_msg_trash(sent3).await;
        assert_eq!(bob_chat_id.get_msg_cnt(&bob).await?, 2);
        let info_msg = bob.get_last_msg().await;
        assert_eq!(info_msg.get_text(), "i2");

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_info_msg_no_cleanup_on_interrupted_series() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "c").await?;
        let instance = send_webxdc_instance(&t, chat_id).await?;

        t.send_webxdc_status_update(instance.id, r#"{"info":"i1", "payload":1}"#, "d")
            .await?;
        assert_eq!(chat_id.get_msg_cnt(&t).await?, 2);
        send_text_msg(&t, chat_id, "msg between info".to_string()).await?;
        assert_eq!(chat_id.get_msg_cnt(&t).await?, 3);
        t.send_webxdc_status_update(instance.id, r#"{"info":"i2", "payload":2}"#, "d")
            .await?;
        assert_eq!(chat_id.get_msg_cnt(&t).await?, 4);

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_opportunistic_encryption() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;

        // Bob sends sth. to Alice, Alice has Bob's key
        let bob_chat_id = create_group_chat(&bob, ProtectionStatus::Unprotected, "chat").await?;
        add_contact_to_chat(
            &bob,
            bob_chat_id,
            Contact::create(&bob, "", "alice@example.org").await?,
        )
        .await?;
        send_text_msg(&bob, bob_chat_id, "populate".to_string()).await?;
        alice.recv_msg(&bob.pop_sent_msg().await).await;

        // Alice sends instance+update to Bob
        let alice_chat_id = alice.get_last_msg().await.chat_id;
        alice_chat_id.accept(&alice).await?;
        let alice_instance = send_webxdc_instance(&alice, alice_chat_id).await?;
        let sent1 = &alice.pop_sent_msg().await;
        alice
            .send_webxdc_status_update(alice_instance.id, r#"{"payload":42}"#, "descr")
            .await?;
        alice.flush_status_updates().await?;
        let sent2 = &alice.pop_sent_msg().await;
        let update_msg = sent2.load_from_db().await;
        assert!(alice_instance.get_showpadlock());
        assert!(update_msg.get_showpadlock());

        // Bob receives instance+update
        let bob_instance = bob.recv_msg(sent1).await;
        bob.recv_msg_trash(sent2).await;
        assert!(bob_instance.get_showpadlock());

        // Bob adds Claire with unknown key, update to Alice+Claire cannot be encrypted
        add_contact_to_chat(
            &bob,
            bob_chat_id,
            Contact::create(&bob, "", "claire@example.org").await?,
        )
        .await?;
        bob.send_webxdc_status_update(bob_instance.id, r#"{"payload":43}"#, "descr")
            .await?;
        bob.flush_status_updates().await?;
        let sent3 = bob.pop_sent_msg().await;
        let update_msg = sent3.load_from_db().await;
        assert!(!update_msg.get_showpadlock());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_internet_access() -> Result<()> {
        let t = TestContext::new_alice().await;
        let self_id = t.get_self_chat().await.id;
        let single_id = t.create_chat_with_contact("bob", "bob@e.com").await.id;
        let group_id = create_group_chat(&t, ProtectionStatus::Unprotected, "chat").await?;
        let broadcast_id = create_broadcast_list(&t).await?;

        let mut first_test = true; // only the first test has all conditions for internet access

        for e2ee in ["1", "0"] {
            t.set_config(Config::E2eeEnabled, Some(e2ee)).await?;
            for chat_id in [self_id, single_id, group_id, broadcast_id] {
                for internet_xdc in [true, false] {
                    let mut instance = create_webxdc_instance(
                        &t,
                        "foo.xdc",
                        if internet_xdc {
                            include_bytes!("../test-data/webxdc/request-internet-access.xdc")
                        } else {
                            include_bytes!("../test-data/webxdc/minimal.xdc")
                        },
                    )
                    .await?;
                    let instance_id = send_msg(&t, chat_id, &mut instance).await?;
                    t.send_webxdc_status_update(
                        instance_id,
                        r#"{"summary":"real summary", "payload": 42}"#,
                        "descr",
                    )
                    .await?;
                    let instance = Message::load_from_db(&t, instance_id).await?;
                    let info = instance.get_webxdc_info(&t).await?;
                    assert_eq!(info.internet_access, first_test);
                    assert_eq!(info.summary.contains("Do not enter sensitive"), first_test);
                    assert_eq!(info.summary.contains("real summary"), !first_test);

                    first_test = false;
                }
            }
        }

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_chatlist_summary() -> Result<()> {
        let t = TestContext::new_alice().await;
        let chat_id = create_group_chat(&t, ProtectionStatus::Unprotected, "chat").await?;
        let mut instance = create_webxdc_instance(
            &t,
            "with-minimal-manifest.xdc",
            include_bytes!("../test-data/webxdc/with-minimal-manifest.xdc"),
        )
        .await?;
        send_msg(&t, chat_id, &mut instance).await?;

        let chatlist = Chatlist::try_load(&t, 0, None, None).await?;
        assert_eq!(chatlist.len(), 1);
        let summary = chatlist.get_summary(&t, 0, None).await?;
        assert_eq!(summary.text, "nice app!".to_string());

        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_and_text() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;

        // Alice sends instance and adds some text
        let alice_chat = alice.create_chat(&bob).await;
        let mut alice_instance = create_webxdc_instance(
            &alice,
            "minimal.xdc",
            include_bytes!("../test-data/webxdc/minimal.xdc"),
        )
        .await?;
        alice_instance.set_text("user added text".to_string());
        send_msg(&alice, alice_chat.id, &mut alice_instance).await?;
        let alice_instance = alice.get_last_msg().await;
        assert_eq!(alice_instance.get_text(), "user added text");

        // Bob receives that instance
        let sent1 = alice.pop_sent_msg().await;
        let bob_instance = bob.recv_msg(&sent1).await;
        assert_eq!(bob_instance.get_text(), "user added text");

        // Alice's second device receives the instance as well
        let alice2 = TestContext::new_alice().await;
        let alice2_instance = alice2.recv_msg(&sent1).await;
        assert_eq!(alice2_instance.get_text(), "user added text");

        Ok(())
    }

    async fn helper_send_receive_status_update(
        bob: &TestContext,
        alice: &TestContext,
        bob_instance: &Message,
        alice_instance: &Message,
    ) -> Result<String> {
        bob.send_webxdc_status_update(
            bob_instance.id,
            r#"{"payload":7,"info": "i","summary":"s"}"#,
            "",
        )
        .await?;
        bob.flush_status_updates().await?;
        let msg = bob.pop_sent_msg().await;
        alice.recv_msg_trash(&msg).await;
        alice
            .get_webxdc_status_updates(alice_instance.id, StatusUpdateSerial(0))
            .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_reject_updates_from_non_groupmembers() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;
        let contact_bob = Contact::create(&alice, "Bob", "bob@example.net").await?;
        let chat_id = create_group_chat(&alice, ProtectionStatus::Unprotected, "Group").await?;
        add_contact_to_chat(&alice, chat_id, contact_bob).await?;
        let instance = send_webxdc_instance(&alice, chat_id).await?;
        bob.recv_msg(&alice.pop_sent_msg().await).await;
        let bob_instance = bob.get_last_msg().await;
        Chat::load_from_db(&bob, bob_instance.chat_id)
            .await?
            .id
            .accept(&bob)
            .await?;

        let status =
            helper_send_receive_status_update(&bob, &alice, &bob_instance, &instance).await?;
        assert_eq!(
            status,
            r#"[{"payload":7,"info":"i","summary":"s","serial":1,"max_serial":1}]"#
        );

        remove_contact_from_chat(&alice, chat_id, contact_bob).await?;
        alice.pop_sent_msg().await;
        let status =
            helper_send_receive_status_update(&bob, &alice, &bob_instance, &instance).await?;

        assert_eq!(
            status,
            r#"[{"payload":7,"info":"i","summary":"s","serial":1,"max_serial":1}]"#
        );
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_webxdc_delete_event() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let chat_id = create_group_chat(&alice, ProtectionStatus::Unprotected, "foo").await?;
        let instance = send_webxdc_instance(&alice, chat_id).await?;
        message::delete_msgs(&alice, &[instance.id]).await?;
        alice
            .evtracker
            .get_matching(|evt| matches!(evt, EventType::WebxdcInstanceDeleted { .. }))
            .await;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn change_logging_webxdc() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let chat_id = ChatId::create_for_contact(&alice, ContactId::SELF).await?;

        assert_eq!(
            alice
                .sql
                .count("SELECT COUNT(*) FROM msgs_status_updates;", ())
                .await?,
            0
        );

        let mut instance = create_webxdc_instance(
            &alice,
            "debug_logging.xdc",
            include_bytes!("../test-data/webxdc/minimal.xdc"),
        )
        .await?;
        assert!(alice.debug_logging.read().unwrap().is_none());
        send_msg(&alice, chat_id, &mut instance).await?;
        assert!(alice.debug_logging.read().unwrap().is_some());

        alice.emit_event(EventType::Info("hi".to_string()));
        alice
            .evtracker
            .get_matching(|ev| matches!(*ev, EventType::WebxdcStatusUpdate { .. }))
            .await;
        assert!(
            alice
                .sql
                .count("SELECT COUNT(*) FROM msgs_status_updates;", ())
                .await?
                > 0
        );
        Ok(())
    }

    /// Tests extensibility of WebXDC updates.
    ///
    /// If an update sent by WebXDC contains unknown properties,
    /// such as `aNewUnknownProperty` or a reserved property
    /// like `serial` or `max_serial`,
    /// they are silently dropped and are not sent over the wire.
    ///
    /// This ensures new WebXDC can try to send new properties
    /// added in later revisions of the WebXDC API
    /// and this will not result in a failure to send the whole update.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_send_webxdc_status_update_extensibility() -> Result<()> {
        let alice = TestContext::new_alice().await;
        let bob = TestContext::new_bob().await;
        let alice_chat = alice.create_chat(&bob).await;
        let alice_instance = send_webxdc_instance(&alice, alice_chat.id).await?;

        let bob_instance = bob.recv_msg(&alice.pop_sent_msg().await).await;

        alice
            .send_webxdc_status_update(
                alice_instance.id,
                r#"{"payload":"p","info":"i","aNewUnknownProperty":"x","max_serial":123}"#,
                "Some description",
            )
            .await?;
        alice.flush_status_updates().await?;
        let received_update = bob.recv_msg_opt(&alice.pop_sent_msg().await).await;
        assert!(received_update.is_none());

        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":"p","info":"i","serial":1,"max_serial":1}]"#
        );

        Ok(())
    }

    // NB: This test also checks that a contact is not marked as bot after receiving from it a
    // webxdc instance and status updates.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_status_update_vs_delete_device_after() -> Result<()> {
        let alice = &TestContext::new_alice().await;
        let bob = &TestContext::new_bob().await;
        bob.set_config(Config::DeleteDeviceAfter, Some("3600"))
            .await?;
        let alice_chat = alice.create_chat(bob).await;
        let alice_instance = send_webxdc_instance(alice, alice_chat.id).await?;
        let bob_instance = bob.recv_msg(&alice.pop_sent_msg().await).await;
        assert_eq!(bob.add_or_lookup_contact(alice).await.is_bot(), false);

        SystemTime::shift(Duration::from_secs(1800));
        let mut update = Message {
            chat_id: alice_chat.id,
            viewtype: Viewtype::Text,
            text: "I'm an update".to_string(),
            hidden: true,
            ..Default::default()
        };
        update.param.set_cmd(SystemMessage::WebxdcStatusUpdate);
        update
            .param
            .set(Param::Arg, r#"{"updates":[{"payload":{"foo":"bar"}}]}"#);
        update.set_quote(alice, Some(&alice_instance)).await?;
        let sent_msg = alice.send_msg(alice_chat.id, &mut update).await;
        bob.recv_msg_trash(&sent_msg).await;
        assert_eq!(bob.add_or_lookup_contact(alice).await.is_bot(), false);
        assert_eq!(
            bob.get_webxdc_status_updates(bob_instance.id, StatusUpdateSerial(0))
                .await?,
            r#"[{"payload":{"foo":"bar"},"serial":1,"max_serial":1}]"#
        );
        assert_eq!(bob.add_or_lookup_contact(alice).await.is_bot(), false);

        SystemTime::shift(Duration::from_secs(2700));
        ephemeral::delete_expired_messages(bob, tools::time()).await?;
        let bob_instance = Message::load_from_db(bob, bob_instance.id).await?;
        assert_eq!(bob_instance.chat_id.is_trash(), false);

        Ok(())
    }
}
