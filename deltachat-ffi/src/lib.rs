#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case
)]

#[macro_use]
extern crate human_panic;
extern crate num_traits;

use num_traits::{FromPrimitive, ToPrimitive};
use std::convert::TryInto;
use std::ptr;
use std::str::FromStr;

use deltachat::contact::Contact;
use deltachat::dc_tools::{as_str, StrExt};
use deltachat::*;

// TODO: constants

// dc_context_t

pub type dc_context_t = context::Context;

pub type dc_callback_t = types::dc_callback_t;

#[no_mangle]
pub unsafe extern "C" fn dc_context_new(
    cb: Option<dc_callback_t>,
    userdata: *mut libc::c_void,
    os_name: *const libc::c_char,
) -> *mut dc_context_t {
    setup_panic!();

    let os_name = if os_name.is_null() {
        None
    } else {
        Some(dc_tools::to_string_lossy(os_name))
    };
    let ctx = context::dc_context_new(cb, userdata, os_name);

    Box::into_raw(Box::new(ctx))
}

/// Release the context structure.
///
/// This function releases the memory of the `dc_context_t` structure.
#[no_mangle]
pub unsafe extern "C" fn dc_context_unref(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &mut *context;
    context::dc_close(context);
    Box::from_raw(context);
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_userdata(context: *mut dc_context_t) -> *mut libc::c_void {
    assert!(!context.is_null());
    let context = &mut *context;

    context::dc_get_userdata(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_open(
    context: *mut dc_context_t,
    dbfile: *mut libc::c_char,
    blobdir: *mut libc::c_char,
) -> libc::c_int {
    assert!(!context.is_null());
    assert!(!dbfile.is_null());
    let context = &mut *context;

    let dbfile_str = dc_tools::as_str(dbfile);
    let blobdir_str = if blobdir.is_null() {
        None
    } else {
        Some(dc_tools::as_str(blobdir))
    };
    context::dc_open(context, dbfile_str, blobdir_str) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_close(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &mut *context;
    context::dc_close(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_is_open(context: *mut dc_context_t) -> libc::c_int {
    assert!(!context.is_null());
    let context = &mut *context;
    context::dc_is_open(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_blobdir(context: *mut dc_context_t) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    context::dc_get_blobdir(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_set_config(
    context: *mut dc_context_t,
    key: *mut libc::c_char,
    value: *mut libc::c_char,
) -> libc::c_int {
    assert!(!context.is_null());
    assert!(!key.is_null(), "invalid key");
    let context = &*context;

    match config::Config::from_str(dc_tools::as_str(key)) {
        Ok(key) => context.set_config(key, as_opt_str(value)).is_ok() as libc::c_int,
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_config(
    context: *mut dc_context_t,
    key: *mut libc::c_char,
) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    assert!(!key.is_null(), "invalid key pointer");
    let key = config::Config::from_str(dc_tools::as_str(key)).expect("invalid key");

    // TODO: Translating None to NULL would be more sensible than translating None
    // to "", as it is now.
    context.get_config(key).unwrap_or_default().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_info(context: *mut dc_context_t) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    context::dc_get_info(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_oauth2_url(
    context: *mut dc_context_t,
    addr: *mut libc::c_char,
    redirect: *mut libc::c_char,
) -> *mut libc::c_char {
    assert!(!context.is_null());

    let context = &*context;
    let addr = dc_tools::to_string(addr);
    let redirect = dc_tools::to_string(redirect);
    match oauth2::dc_get_oauth2_url(context, addr, redirect) {
        Some(res) => res.strdup(),
        None => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_version_str() -> *mut libc::c_char {
    context::dc_get_version_str()
}

#[no_mangle]
pub unsafe extern "C" fn dc_configure(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    configure::configure(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_is_configured(context: *mut dc_context_t) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    configure::dc_is_configured(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_imap_jobs(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_imap_jobs(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_imap_fetch(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_imap_fetch(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_imap_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_imap_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_interrupt_imap_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::interrupt_imap_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_mvbox_fetch(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_mvbox_fetch(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_mvbox_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_mvbox_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_interrupt_mvbox_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::interrupt_mvbox_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_sentbox_fetch(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_sentbox_fetch(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_sentbox_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_sentbox_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_interrupt_sentbox_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::interrupt_sentbox_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_smtp_jobs(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_smtp_jobs(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_perform_smtp_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::perform_smtp_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_interrupt_smtp_idle(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::interrupt_smtp_idle(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_maybe_network(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    job::maybe_network(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_chatlist<'a>(
    context: *mut dc_context_t,
    flags: libc::c_int,
    query_str: *mut libc::c_char,
    query_id: u32,
) -> *mut dc_chatlist_t<'a> {
    assert!(!context.is_null());
    let context = &*context;

    let qs = if query_str.is_null() {
        None
    } else {
        Some(dc_tools::as_str(query_str))
    };
    let qi = if query_id == 0 { None } else { Some(query_id) };
    match chatlist::Chatlist::try_load(context, flags as usize, qs, qi) {
        Ok(list) => Box::into_raw(Box::new(list)),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_create_chat_by_msg_id(context: *mut dc_context_t, msg_id: u32) -> u32 {
    assert!(!context.is_null());
    let context = &*context;

    chat::create_by_msg_id(context, msg_id).unwrap_or_log_default(context, "Failed to create chat")
}

#[no_mangle]
pub unsafe extern "C" fn dc_create_chat_by_contact_id(
    context: *mut dc_context_t,
    contact_id: u32,
) -> u32 {
    assert!(!context.is_null());
    let context = &*context;

    chat::create_by_contact_id(context, contact_id)
        .unwrap_or_log_default(context, "Failed to create chat")
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_chat_id_by_contact_id(
    context: *mut dc_context_t,
    contact_id: u32,
) -> u32 {
    assert!(!context.is_null());
    let context = &*context;

    chat::get_by_contact_id(context, contact_id)
        .unwrap_or_log_default(context, "Failed to get chat")
}

#[no_mangle]
pub unsafe extern "C" fn dc_prepare_msg(
    context: *mut dc_context_t,
    chat_id: u32,
    msg: *mut dc_msg_t,
) -> u32 {
    assert!(!context.is_null());
    assert!(!msg.is_null());
    let context = &mut *context;

    let msg = &mut *msg;
    chat::prepare_msg(context, chat_id, msg)
        .unwrap_or_log_default(context, "Failed to prepare message")
}

#[no_mangle]
pub unsafe extern "C" fn dc_send_msg(
    context: *mut dc_context_t,
    chat_id: u32,
    msg: *mut dc_msg_t,
) -> u32 {
    assert!(!context.is_null());
    assert!(!msg.is_null());
    let context = &mut *context;
    let msg = &mut *msg;

    chat::send_msg(context, chat_id, msg).unwrap_or_log_default(context, "Failed to send message")
}

#[no_mangle]
pub unsafe extern "C" fn dc_send_text_msg(
    context: *mut dc_context_t,
    chat_id: u32,
    text_to_send: *mut libc::c_char,
) -> u32 {
    assert!(!context.is_null());
    assert!(!text_to_send.is_null());
    let context = &*context;
    let text_to_send = dc_tools::to_string_lossy(text_to_send);

    chat::send_text_msg(context, chat_id, text_to_send)
        .unwrap_or_log_default(context, "Failed to send text message")
}

#[no_mangle]
pub unsafe extern "C" fn dc_set_draft(
    context: *mut dc_context_t,
    chat_id: u32,
    msg: *mut dc_msg_t,
) {
    assert!(!context.is_null());
    let context = &*context;
    let msg = if msg.is_null() { None } else { Some(&mut *msg) };

    chat::set_draft(context, chat_id, msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_draft<'a>(
    context: *mut dc_context_t,
    chat_id: u32,
) -> *mut dc_msg_t<'a> {
    assert!(!context.is_null());
    let context = &*context;

    chat::get_draft(context, chat_id).into_raw()
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_chat_msgs(
    context: *mut dc_context_t,
    chat_id: u32,
    flags: u32,
    marker1before: u32,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    let context = &*context;

    let arr = dc_array_t::from(chat::get_chat_msgs(context, chat_id, flags, marker1before));
    Box::into_raw(Box::new(arr))
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_msg_cnt(context: *mut dc_context_t, chat_id: u32) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    chat::get_msg_cnt(context, chat_id) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_fresh_msg_cnt(
    context: *mut dc_context_t,
    chat_id: u32,
) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    chat::get_fresh_msg_cnt(context, chat_id) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_fresh_msgs(
    context: *mut dc_context_t,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    let context = &*context;

    let arr = dc_array_t::from(context::dc_get_fresh_msgs(context));
    Box::into_raw(Box::new(arr))
}

#[no_mangle]
pub unsafe extern "C" fn dc_marknoticed_chat(context: *mut dc_context_t, chat_id: u32) {
    assert!(!context.is_null());
    let context = &*context;

    chat::marknoticed_chat(context, chat_id).log_err(context, "Failed marknoticed chat");
}

#[no_mangle]
pub unsafe extern "C" fn dc_marknoticed_all_chats(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    chat::marknoticed_all_chats(context).log_err(context, "Failed marknoticed all chats");
}

fn from_prim<S, T>(s: S) -> Option<T>
where
    T: FromPrimitive,
    S: Into<i64>,
{
    FromPrimitive::from_i64(s.into())
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_chat_media(
    context: *mut dc_context_t,
    chat_id: u32,
    msg_type: libc::c_int,
    or_msg_type2: libc::c_int,
    or_msg_type3: libc::c_int,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    let context = &*context;

    let msg_type = from_prim(msg_type).expect(&format!("invalid msg_type = {}", msg_type));
    let or_msg_type2 =
        from_prim(or_msg_type2).expect(&format!("incorrect or_msg_type2 = {}", or_msg_type2));
    let or_msg_type3 =
        from_prim(or_msg_type3).expect(&format!("incorrect or_msg_type3 = {}", or_msg_type3));

    let arr = dc_array_t::from(chat::get_chat_media(
        context,
        chat_id,
        msg_type,
        or_msg_type2,
        or_msg_type3,
    ));
    Box::into_raw(Box::new(arr))
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_next_media(
    context: *mut dc_context_t,
    msg_id: u32,
    dir: libc::c_int,
    msg_type: libc::c_int,
    or_msg_type2: libc::c_int,
    or_msg_type3: libc::c_int,
) -> u32 {
    assert!(!context.is_null());
    let context = &*context;

    let msg_type = from_prim(msg_type).expect(&format!("invalid msg_type = {}", msg_type));
    let or_msg_type2 =
        from_prim(or_msg_type2).expect(&format!("incorrect or_msg_type2 = {}", or_msg_type2));
    let or_msg_type3 =
        from_prim(or_msg_type3).expect(&format!("incorrect or_msg_type3 = {}", or_msg_type3));

    chat::get_next_media(context, msg_id, dir, msg_type, or_msg_type2, or_msg_type3)
}

#[no_mangle]
pub unsafe extern "C" fn dc_archive_chat(
    context: *mut dc_context_t,
    chat_id: u32,
    archive: libc::c_int,
) {
    assert!(!context.is_null());
    let context = &*context;

    let archive = if archive == 0 {
        false
    } else if archive == 1 {
        true
    } else {
        return;
    };

    chat::archive(context, chat_id, archive).log_err(context, "Failed archive chat");
}

#[no_mangle]
pub unsafe extern "C" fn dc_delete_chat(context: *mut dc_context_t, chat_id: u32) {
    assert!(!context.is_null());
    let context = &*context;

    chat::delete(context, chat_id).log_err(context, "Failed chat delete");
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_chat_contacts(
    context: *mut dc_context_t,
    chat_id: u32,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    let context = &*context;

    let arr = dc_array_t::from(chat::get_chat_contacts(context, chat_id));
    Box::into_raw(Box::new(arr))
}

#[no_mangle]
pub unsafe extern "C" fn dc_search_msgs(
    context: *mut dc_context_t,
    chat_id: u32,
    query: *mut libc::c_char,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    assert!(!query.is_null());
    let context = &*context;

    let arr = dc_array_t::from(context::dc_search_msgs(context, chat_id, query));
    Box::into_raw(Box::new(arr))
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_chat<'a>(
    context: *mut dc_context_t,
    chat_id: u32,
) -> *mut dc_chat_t<'a> {
    assert!(!context.is_null());
    let context = &*context;

    match chat::Chat::load_from_db(context, chat_id) {
        Ok(chat) => Box::into_raw(Box::new(chat)),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_create_group_chat(
    context: *mut dc_context_t,
    verified: libc::c_int,
    name: *mut libc::c_char,
) -> u32 {
    assert!(!context.is_null());
    assert!(!name.is_null());
    let context = &*context;

    let verified = if let Some(s) = contact::VerifiedStatus::from_i32(verified) {
        s
    } else {
        return 0;
    };

    chat::create_group_chat(context, verified, as_str(name))
        .unwrap_or_log_default(context, "Failed to create group chat")
}

#[no_mangle]
pub unsafe extern "C" fn dc_is_contact_in_chat(
    context: *mut dc_context_t,
    chat_id: u32,
    contact_id: u32,
) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    chat::is_contact_in_chat(context, chat_id, contact_id)
}

#[no_mangle]
pub unsafe extern "C" fn dc_add_contact_to_chat(
    context: *mut dc_context_t,
    chat_id: u32,
    contact_id: u32,
) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    chat::add_contact_to_chat(context, chat_id, contact_id)
}

#[no_mangle]
pub unsafe extern "C" fn dc_remove_contact_from_chat(
    context: *mut dc_context_t,
    chat_id: u32,
    contact_id: u32,
) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    chat::remove_contact_from_chat(context, chat_id, contact_id)
        .map(|_| 1)
        .unwrap_or_log_default(context, "Failed to remove contact")
}

#[no_mangle]
pub unsafe extern "C" fn dc_set_chat_name(
    context: *mut dc_context_t,
    chat_id: u32,
    name: *mut libc::c_char,
) -> libc::c_int {
    assert!(!context.is_null());
    assert!(!name.is_null());
    assert!(chat_id > constants::DC_CHAT_ID_LAST_SPECIAL as u32);
    let context = &*context;

    chat::set_chat_name(context, chat_id, as_str(name))
        .map(|_| 1)
        .unwrap_or_log_default(context, "Failed to set chat name")
}

#[no_mangle]
pub unsafe extern "C" fn dc_set_chat_profile_image(
    context: *mut dc_context_t,
    chat_id: u32,
    image: *mut libc::c_char,
) -> libc::c_int {
    assert!(!context.is_null());
    assert!(chat_id > constants::DC_CHAT_ID_LAST_SPECIAL as u32);
    let context = &*context;

    chat::set_chat_profile_image(context, chat_id, as_str(image))
        .map(|_| 1)
        .unwrap_or_log_default(context, "Failed to set profile image")
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_msg_info(
    context: *mut dc_context_t,
    msg_id: u32,
) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    message::dc_get_msg_info(context, msg_id)
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_mime_headers(
    context: *mut dc_context_t,
    msg_id: u32,
) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    message::dc_get_mime_headers(context, msg_id)
}

#[no_mangle]
pub unsafe extern "C" fn dc_delete_msgs(
    context: *mut dc_context_t,
    msg_ids: *const u32,
    msg_cnt: libc::c_int,
) {
    assert!(!context.is_null());
    assert!(!msg_ids.is_null());
    assert!(msg_cnt > 0);
    let context = &*context;

    message::dc_delete_msgs(context, msg_ids, msg_cnt)
}

#[no_mangle]
pub unsafe extern "C" fn dc_forward_msgs(
    context: *mut dc_context_t,
    msg_ids: *const u32,
    msg_cnt: libc::c_int,
    chat_id: u32,
) {
    assert!(!context.is_null());
    assert!(!msg_ids.is_null());
    assert!(msg_cnt > 0);
    assert!(chat_id > constants::DC_CHAT_ID_LAST_SPECIAL as u32);
    let context = &*context;

    chat::forward_msgs(context, msg_ids, msg_cnt, chat_id)
}

#[no_mangle]
pub unsafe extern "C" fn dc_marknoticed_contact(context: *mut dc_context_t, contact_id: u32) {
    assert!(!context.is_null());
    let context = &*context;

    Contact::mark_noticed(context, contact_id)
}

#[no_mangle]
pub unsafe extern "C" fn dc_markseen_msgs(
    context: *mut dc_context_t,
    msg_ids: *const u32,
    msg_cnt: libc::c_int,
) {
    assert!(!context.is_null());
    assert!(!msg_ids.is_null());
    assert!(msg_cnt > 0);
    let context = &*context;

    message::dc_markseen_msgs(context, msg_ids, msg_cnt as usize);
}

#[no_mangle]
pub unsafe extern "C" fn dc_star_msgs(
    context: *mut dc_context_t,
    msg_ids: *const u32,
    msg_cnt: libc::c_int,
    star: libc::c_int,
) {
    assert!(!context.is_null());
    assert!(!msg_ids.is_null());
    assert!(msg_cnt > 0);

    let context = &*context;

    message::dc_star_msgs(context, msg_ids, msg_cnt, star);
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_msg<'a>(
    context: *mut dc_context_t,
    msg_id: u32,
) -> *mut dc_msg_t<'a> {
    assert!(!context.is_null());
    let context = &*context;

    message::dc_get_msg(context, msg_id).into_raw()
}

#[no_mangle]
pub unsafe extern "C" fn dc_may_be_valid_addr(addr: *mut libc::c_char) -> libc::c_int {
    assert!(!addr.is_null());
    contact::may_be_valid_addr(as_str(addr)) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_lookup_contact_id_by_addr(
    context: *mut dc_context_t,
    addr: *mut libc::c_char,
) -> u32 {
    assert!(!context.is_null());
    assert!(!addr.is_null());
    let context = &*context;

    Contact::lookup_id_by_addr(context, as_str(addr))
}

#[no_mangle]
pub unsafe extern "C" fn dc_create_contact(
    context: *mut dc_context_t,
    name: *mut libc::c_char,
    addr: *mut libc::c_char,
) -> u32 {
    assert!(!context.is_null());
    assert!(!addr.is_null());

    let context = &*context;

    let name = if name.is_null() { "" } else { as_str(name) };

    match Contact::create(context, name, as_str(addr)) {
        Ok(id) => id,
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_add_address_book(
    context: *mut dc_context_t,
    addr_book: *mut libc::c_char,
) -> libc::c_int {
    assert!(!context.is_null());
    assert!(!addr_book.is_null());
    let context = &*context;

    match Contact::add_address_book(context, as_str(addr_book)) {
        Ok(cnt) => cnt as libc::c_int,
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_contacts(
    context: *mut dc_context_t,
    flags: u32,
    query: *mut libc::c_char,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    let context = &*context;

    let query = if query.is_null() {
        None
    } else {
        Some(as_str(query))
    };

    match Contact::get_all(context, flags, query) {
        Ok(contacts) => Box::into_raw(Box::new(dc_array_t::from(contacts))),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_blocked_cnt(context: *mut dc_context_t) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    Contact::get_blocked_cnt(context) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_blocked_contacts(
    context: *mut dc_context_t,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    let context = &*context;

    Box::into_raw(Box::new(dc_array_t::from(Contact::get_all_blocked(
        context,
    ))))
}

#[no_mangle]
pub unsafe extern "C" fn dc_block_contact(
    context: *mut dc_context_t,
    contact_id: u32,
    block: libc::c_int,
) {
    assert!(!context.is_null());
    let context = &*context;

    if block == 0 {
        Contact::unblock(context, contact_id);
    } else {
        Contact::block(context, contact_id);
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_contact_encrinfo(
    context: *mut dc_context_t,
    contact_id: u32,
) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    Contact::get_encrinfo(context, contact_id)
        .map(|s| s.strdup())
        .unwrap_or_else(|e| {
            error!(context, 0, "{}", e);
            std::ptr::null_mut()
        })
}

#[no_mangle]
pub unsafe extern "C" fn dc_delete_contact(
    context: *mut dc_context_t,
    contact_id: u32,
) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    match Contact::delete(context, contact_id) {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_contact<'a>(
    context: *mut dc_context_t,
    contact_id: u32,
) -> *mut dc_contact_t<'a> {
    assert!(!context.is_null());
    let context = &*context;

    Contact::get_by_id(context, contact_id)
        .map(|contact| Box::into_raw(Box::new(contact)))
        .unwrap_or_else(|_| std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn dc_imex(
    context: *mut dc_context_t,
    what: libc::c_int,
    param1: *mut libc::c_char,
    param2: *mut libc::c_char,
) {
    assert!(!context.is_null());
    let context = &*context;

    dc_imex::dc_imex(context, what, param1, param2)
}

#[no_mangle]
pub unsafe extern "C" fn dc_imex_has_backup(
    context: *mut dc_context_t,
    dir: *mut libc::c_char,
) -> *mut libc::c_char {
    assert!(!context.is_null());
    assert!(!dir.is_null());
    let context = &*context;

    dc_imex::dc_imex_has_backup(context, dir)
}

#[no_mangle]
pub unsafe extern "C" fn dc_initiate_key_transfer(context: *mut dc_context_t) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    dc_imex::dc_initiate_key_transfer(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_continue_key_transfer(
    context: *mut dc_context_t,
    msg_id: u32,
    setup_code: *mut libc::c_char,
) -> libc::c_int {
    assert!(!context.is_null());
    assert!(!setup_code.is_null());
    let context = &*context;

    dc_imex::dc_continue_key_transfer(context, msg_id, setup_code)
}

#[no_mangle]
pub unsafe extern "C" fn dc_stop_ongoing_process(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    configure::dc_stop_ongoing_process(context)
}

#[no_mangle]
pub unsafe extern "C" fn dc_check_qr(
    context: *mut dc_context_t,
    qr: *mut libc::c_char,
) -> *mut dc_lot_t {
    assert!(!context.is_null());
    assert!(!qr.is_null());
    let context = &*context;

    let lot = qr::check_qr(context, as_str(qr));
    Box::into_raw(Box::new(lot))
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_securejoin_qr(
    context: *mut dc_context_t,
    chat_id: u32,
) -> *mut libc::c_char {
    assert!(!context.is_null());
    let context = &*context;

    dc_securejoin::dc_get_securejoin_qr(context, chat_id)
}

#[no_mangle]
pub unsafe extern "C" fn dc_join_securejoin(
    context: *mut dc_context_t,
    qr: *mut libc::c_char,
) -> u32 {
    assert!(!context.is_null());
    assert!(!qr.is_null());
    let context = &*context;

    dc_securejoin::dc_join_securejoin(context, qr)
}

#[no_mangle]
pub unsafe extern "C" fn dc_send_locations_to_chat(
    context: *mut dc_context_t,
    chat_id: u32,
    seconds: libc::c_int,
) {
    assert!(!context.is_null());
    let context = &*context;

    location::send_locations_to_chat(context, chat_id, seconds as i64)
}

#[no_mangle]
pub unsafe extern "C" fn dc_is_sending_locations_to_chat(
    context: *mut dc_context_t,
    chat_id: u32,
) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    location::is_sending_locations_to_chat(context, chat_id) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_set_location(
    context: *mut dc_context_t,
    latitude: libc::c_double,
    longitude: libc::c_double,
    accuracy: libc::c_double,
) -> libc::c_int {
    assert!(!context.is_null());
    let context = &*context;

    location::set(context, latitude, longitude, accuracy)
}

#[no_mangle]
pub unsafe extern "C" fn dc_get_locations(
    context: *mut dc_context_t,
    chat_id: u32,
    contact_id: u32,
    timestamp_begin: i64,
    timestamp_end: i64,
) -> *mut dc_array::dc_array_t {
    assert!(!context.is_null());
    let context = &*context;

    let res = location::get_range(
        context,
        chat_id,
        contact_id,
        timestamp_begin as i64,
        timestamp_end as i64,
    );
    Box::into_raw(Box::new(dc_array_t::from(res)))
}

#[no_mangle]
pub unsafe extern "C" fn dc_delete_all_locations(context: *mut dc_context_t) {
    assert!(!context.is_null());
    let context = &*context;

    location::delete_all(context);
}

// dc_array_t

#[no_mangle]
pub type dc_array_t = dc_array::dc_array_t;

#[no_mangle]
pub unsafe extern "C" fn dc_array_unref(a: *mut dc_array::dc_array_t) {
    assert!(!a.is_null());

    Box::from_raw(a);
}

#[no_mangle]
pub unsafe extern "C" fn dc_array_add_id(array: *mut dc_array_t, item: libc::c_uint) {
    assert!(!array.is_null());

    (*array).add_id(item);
}

#[no_mangle]
pub unsafe extern "C" fn dc_array_get_cnt(array: *const dc_array_t) -> libc::size_t {
    assert!(!array.is_null());

    (*array).len()
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_id(array: *const dc_array_t, index: libc::size_t) -> u32 {
    assert!(!array.is_null());

    (*array).get_id(index)
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_latitude(
    array: *const dc_array_t,
    index: libc::size_t,
) -> libc::c_double {
    assert!(!array.is_null());

    (*array).get_location(index).latitude
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_longitude(
    array: *const dc_array_t,
    index: libc::size_t,
) -> libc::c_double {
    assert!(!array.is_null());

    (*array).get_location(index).longitude
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_accuracy(
    array: *const dc_array_t,
    index: libc::size_t,
) -> libc::c_double {
    assert!(!array.is_null());

    (*array).get_location(index).accuracy
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_timestamp(
    array: *const dc_array_t,
    index: libc::size_t,
) -> i64 {
    assert!(!array.is_null());

    (*array).get_location(index).timestamp
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_chat_id(
    array: *const dc_array_t,
    index: libc::size_t,
) -> libc::c_uint {
    assert!(!array.is_null());

    (*array).get_location(index).chat_id
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_contact_id(
    array: *const dc_array_t,
    index: libc::size_t,
) -> libc::c_uint {
    assert!(!array.is_null());

    (*array).get_location(index).contact_id
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_msg_id(
    array: *const dc_array_t,
    index: libc::size_t,
) -> libc::c_uint {
    assert!(!array.is_null());

    (*array).get_location(index).msg_id
}
#[no_mangle]
pub unsafe extern "C" fn dc_array_get_marker(
    array: *const dc_array_t,
    index: libc::size_t,
) -> *mut libc::c_char {
    assert!(!array.is_null());

    if let Some(s) = &(*array).get_location(index).marker {
        s.strdup()
    } else {
        std::ptr::null_mut()
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_array_search_id(
    array: *const dc_array_t,
    needle: libc::c_uint,
    ret_index: *mut libc::size_t,
) -> libc::c_int {
    assert!(!array.is_null());

    if let Some(i) = (*array).search_id(needle) {
        if !ret_index.is_null() {
            *ret_index = i
        }
        1
    } else {
        0
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_array_get_raw(array: *const dc_array_t) -> *const u32 {
    assert!(!array.is_null());

    (*array).as_ptr()
}

// Return the independent-state of the location at the given index.
// Independent locations do not belong to the track of the user.
// Returns 1 if location belongs to the track of the user,
// 0 if location was reported independently.
#[no_mangle]
pub unsafe fn dc_array_is_independent(
    array: *const dc_array_t,
    index: libc::size_t,
) -> libc::c_int {
    assert!(!array.is_null());

    (*array).get_location(index).independent as libc::c_int
}

// dc_chatlist_t

#[no_mangle]
pub type dc_chatlist_t<'a> = chatlist::Chatlist<'a>;

#[no_mangle]
pub unsafe extern "C" fn dc_chatlist_unref(chatlist: *mut dc_chatlist_t) {
    assert!(!chatlist.is_null());

    Box::from_raw(chatlist);
}

#[no_mangle]
pub unsafe extern "C" fn dc_chatlist_get_cnt(chatlist: *mut dc_chatlist_t) -> libc::size_t {
    assert!(!chatlist.is_null());

    let list = &*chatlist;
    list.len() as libc::size_t
}

#[no_mangle]
pub unsafe extern "C" fn dc_chatlist_get_chat_id(
    chatlist: *mut dc_chatlist_t,
    index: libc::size_t,
) -> u32 {
    assert!(!chatlist.is_null());

    let list = &*chatlist;
    list.get_chat_id(index as usize)
}

#[no_mangle]
pub unsafe extern "C" fn dc_chatlist_get_msg_id(
    chatlist: *mut dc_chatlist_t,
    index: libc::size_t,
) -> u32 {
    assert!(!chatlist.is_null());

    let list = &*chatlist;
    list.get_msg_id(index as usize)
}

#[no_mangle]
pub unsafe extern "C" fn dc_chatlist_get_summary<'a>(
    chatlist: *mut dc_chatlist_t<'a>,
    index: libc::size_t,
    chat: *mut dc_chat_t<'a>,
) -> *mut dc_lot_t {
    assert!(!chatlist.is_null());

    let chat = if chat.is_null() { None } else { Some(&*chat) };
    let list = &*chatlist;

    let lot = list.get_summary(index as usize, chat);
    Box::into_raw(Box::new(lot))
}

#[no_mangle]
pub unsafe extern "C" fn dc_chatlist_get_context(
    chatlist: *mut dc_chatlist_t,
) -> *const dc_context_t {
    assert!(!chatlist.is_null());
    let list = &*chatlist;

    list.get_context() as *const _
}

// dc_chat_t

#[no_mangle]
pub type dc_chat_t<'a> = chat::Chat<'a>;

#[no_mangle]
pub unsafe extern "C" fn dc_chat_unref(chat: *mut dc_chat_t) {
    assert!(!chat.is_null());

    Box::from_raw(chat);
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_get_id(chat: *mut dc_chat_t) -> u32 {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.get_id()
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_get_type(chat: *mut dc_chat_t) -> libc::c_int {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.get_type() as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_get_name(chat: *mut dc_chat_t) -> *mut libc::c_char {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.get_name().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_get_subtitle(chat: *mut dc_chat_t) -> *mut libc::c_char {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.get_subtitle().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_get_profile_image(chat: *mut dc_chat_t) -> *mut libc::c_char {
    assert!(!chat.is_null());
    let chat = &*chat;

    match chat.get_profile_image() {
        Some(i) => i.strdup(),
        None => ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_get_color(chat: *mut dc_chat_t) -> u32 {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.get_color()
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_get_archived(chat: *mut dc_chat_t) -> libc::c_int {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.is_archived() as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_is_unpromoted(chat: *mut dc_chat_t) -> libc::c_int {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.is_unpromoted() as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_is_self_talk(chat: *mut dc_chat_t) -> libc::c_int {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.is_self_talk() as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_is_verified(chat: *mut dc_chat_t) -> libc::c_int {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.is_verified() as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_chat_is_sending_locations(chat: *mut dc_chat_t) -> libc::c_int {
    assert!(!chat.is_null());
    let chat = &*chat;

    chat.is_sending_locations() as libc::c_int
}

// dc_msg_t

#[no_mangle]
pub type dc_msg_t<'a> = message::Message<'a>;

#[no_mangle]
pub unsafe extern "C" fn dc_msg_new<'a>(
    context: *mut dc_context_t,
    viewtype: libc::c_int,
) -> *mut dc_msg_t<'a> {
    assert!(!context.is_null());
    let context = &*context;
    let viewtype = from_prim(viewtype).expect(&format!("invalid viewtype = {}", viewtype));

    Box::into_raw(Box::new(message::dc_msg_new(context, viewtype)))
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_unref(msg: *mut dc_msg_t) {
    assert!(!msg.is_null());

    Box::from_raw(msg);
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_id(msg: *mut dc_msg_t) -> u32 {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_id(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_from_id(msg: *mut dc_msg_t) -> u32 {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_from_id(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_chat_id(msg: *mut dc_msg_t) -> u32 {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_chat_id(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_viewtype(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_viewtype(msg)
        .to_i64()
        .expect("impossible: Viewtype -> i64 conversion failed") as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_state(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_state(msg) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_timestamp(msg: *mut dc_msg_t) -> i64 {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_timestamp(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_received_timestamp(msg: *mut dc_msg_t) -> i64 {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_received_timestamp(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_sort_timestamp(msg: *mut dc_msg_t) -> i64 {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_sort_timestamp(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_text(msg: *mut dc_msg_t) -> *mut libc::c_char {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_text(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_file(msg: *mut dc_msg_t) -> *mut libc::c_char {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_file(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_filename(msg: *mut dc_msg_t) -> *mut libc::c_char {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_filename(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_filemime(msg: *mut dc_msg_t) -> *mut libc::c_char {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_filemime(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_filebytes(msg: *mut dc_msg_t) -> u64 {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_filebytes(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_width(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_width(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_height(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_height(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_duration(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_duration(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_showpadlock(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_showpadlock(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_summary<'a>(
    msg: *mut dc_msg_t<'a>,
    chat: *mut dc_chat_t<'a>,
) -> *mut dc_lot_t {
    assert!(!msg.is_null());
    let chat = if chat.is_null() { None } else { Some(&*chat) };

    let msg = &mut *msg;

    let lot = message::dc_msg_get_summary(msg, chat);
    Box::into_raw(Box::new(lot))
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_summarytext(
    msg: *mut dc_msg_t,
    approx_characters: libc::c_int,
) -> *mut libc::c_char {
    assert!(!msg.is_null());

    let msg = &mut *msg;
    message::dc_msg_get_summarytext(msg, approx_characters.try_into().unwrap())
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_has_deviating_timestamp(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_has_deviating_timestamp(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_has_location(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_has_location(msg) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_is_sent(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_is_sent(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_is_starred(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_is_starred(msg).into()
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_is_forwarded(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_is_forwarded(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_is_info(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_is_info(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_is_increation(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_is_increation(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_is_setupmessage(msg: *mut dc_msg_t) -> libc::c_int {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_is_setupmessage(msg) as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_get_setupcodebegin(msg: *mut dc_msg_t) -> *mut libc::c_char {
    assert!(!msg.is_null());

    let msg = &*msg;
    message::dc_msg_get_setupcodebegin(msg)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_set_text(msg: *mut dc_msg_t, text: *mut libc::c_char) {
    assert!(!msg.is_null());

    let msg = &mut *msg;
    // TODO: {text} equal to NULL is treated as "", which is strange. Does anyone rely on it?
    message::dc_msg_set_text(msg, text)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_set_file(
    msg: *mut dc_msg_t,
    file: *mut libc::c_char,
    filemime: *mut libc::c_char,
) {
    assert!(!msg.is_null());

    let msg = &mut *msg;
    message::dc_msg_set_file(msg, file, filemime)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_set_dimension(
    msg: *mut dc_msg_t,
    width: libc::c_int,
    height: libc::c_int,
) {
    assert!(!msg.is_null());

    let msg = &mut *msg;
    message::dc_msg_set_dimension(msg, width, height)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_set_duration(msg: *mut dc_msg_t, duration: libc::c_int) {
    assert!(!msg.is_null());

    let msg = &mut *msg;
    message::dc_msg_set_duration(msg, duration)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_set_location(
    msg: *mut dc_msg_t,
    latitude: libc::c_double,
    longitude: libc::c_double,
) {
    assert!(!msg.is_null());

    let msg = &mut *msg;
    message::dc_msg_set_location(msg, latitude, longitude)
}

#[no_mangle]
pub unsafe extern "C" fn dc_msg_latefiling_mediasize(
    msg: *mut dc_msg_t,
    width: libc::c_int,
    height: libc::c_int,
    duration: libc::c_int,
) {
    assert!(!msg.is_null());

    let msg = &mut *msg;
    message::dc_msg_latefiling_mediasize(msg, width, height, duration)
}

// dc_contact_t

#[no_mangle]
pub type dc_contact_t<'a> = contact::Contact<'a>;

#[no_mangle]
pub unsafe extern "C" fn dc_contact_unref(contact: *mut dc_contact_t) {
    assert!(!contact.is_null());
    Box::from_raw(contact);
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_id(contact: *mut dc_contact_t) -> u32 {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.get_id()
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_addr(contact: *mut dc_contact_t) -> *mut libc::c_char {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.get_addr().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_name(contact: *mut dc_contact_t) -> *mut libc::c_char {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.get_name().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_display_name(
    contact: *mut dc_contact_t,
) -> *mut libc::c_char {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.get_display_name().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_name_n_addr(
    contact: *mut dc_contact_t,
) -> *mut libc::c_char {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.get_name_n_addr().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_first_name(
    contact: *mut dc_contact_t,
) -> *mut libc::c_char {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.get_first_name().strdup()
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_profile_image(
    contact: *mut dc_contact_t,
) -> *mut libc::c_char {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact
        .get_profile_image()
        .map(|s| s.strdup())
        .unwrap_or_else(|| std::ptr::null_mut())
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_get_color(contact: *mut dc_contact_t) -> u32 {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.get_color()
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_is_blocked(contact: *mut dc_contact_t) -> libc::c_int {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.is_blocked() as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_contact_is_verified(contact: *mut dc_contact_t) -> libc::c_int {
    assert!(!contact.is_null());
    let contact = &*contact;

    contact.is_verified() as libc::c_int
}

// dc_lot_t

#[no_mangle]
pub type dc_lot_t = lot::Lot;

#[no_mangle]
pub unsafe extern "C" fn dc_lot_unref(lot: *mut dc_lot_t) {
    assert!(!lot.is_null());

    Box::from_raw(lot);
}

#[no_mangle]
pub unsafe extern "C" fn dc_lot_get_text1(lot: *mut dc_lot_t) -> *mut libc::c_char {
    assert!(!lot.is_null());

    let lot = &*lot;
    strdup_opt(lot.get_text1())
}

#[no_mangle]
pub unsafe extern "C" fn dc_lot_get_text2(lot: *mut dc_lot_t) -> *mut libc::c_char {
    assert!(!lot.is_null());

    let lot = &*lot;
    strdup_opt(lot.get_text2())
}

#[no_mangle]
pub unsafe extern "C" fn dc_lot_get_text1_meaning(lot: *mut dc_lot_t) -> libc::c_int {
    assert!(!lot.is_null());

    let lot = &*lot;
    lot.get_text1_meaning() as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_lot_get_state(lot: *mut dc_lot_t) -> libc::c_int {
    assert!(!lot.is_null());

    let lot = &*lot;
    lot.get_state().to_i64().expect("impossible") as libc::c_int
}

#[no_mangle]
pub unsafe extern "C" fn dc_lot_get_id(lot: *mut dc_lot_t) -> u32 {
    assert!(!lot.is_null());

    let lot = &*lot;
    lot.get_id()
}

#[no_mangle]
pub unsafe extern "C" fn dc_lot_get_timestamp(lot: *mut dc_lot_t) -> i64 {
    assert!(!lot.is_null());

    let lot = &*lot;
    lot.get_timestamp()
}

#[no_mangle]
pub unsafe extern "C" fn dc_str_unref(s: *mut libc::c_char) {
    libc::free(s as *mut _)
}

fn as_opt_str<'a>(s: *const libc::c_char) -> Option<&'a str> {
    if s.is_null() {
        return None;
    }

    Some(dc_tools::as_str(s))
}

pub trait ResultExt<T> {
    fn unwrap_or_log_default(self, context: &context::Context, message: &str) -> T;
    fn log_err(&self, context: &context::Context, message: &str);
}

impl<T: Default, E: std::fmt::Display> ResultExt<T> for Result<T, E> {
    fn unwrap_or_log_default(self, context: &context::Context, message: &str) -> T {
        match self {
            Ok(t) => t,
            Err(err) => {
                error!(context, 0, "{}: {}", message, err);
                Default::default()
            }
        }
    }

    fn log_err(&self, context: &context::Context, message: &str) {
        if let Err(err) = self {
            error!(context, 0, "{}: {}", message, err);
        }
    }
}

unsafe fn strdup_opt(s: Option<impl AsRef<str>>) -> *mut libc::c_char {
    match s {
        Some(s) => s.as_ref().strdup(),
        None => ptr::null_mut(),
    }
}

pub trait ResultNullableExt<T> {
    fn into_raw(self) -> *mut T;
}

impl<T, E> ResultNullableExt<T> for Result<T, E> {
    fn into_raw(self) -> *mut T {
        match self {
            Ok(t) => Box::into_raw(Box::new(t)),
            Err(_) => ptr::null_mut(),
        }
    }
}
