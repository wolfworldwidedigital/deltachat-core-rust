//! End-to-end decryption support.

use std::collections::HashMap;
use std::collections::HashSet;

use anyhow::{Context as _, Result};
use mailparse::MailHeaderMap;
use mailparse::ParsedMail;

use crate::aheader::Aheader;
use crate::contact::addr_cmp;
use crate::context::Context;
use crate::headerdef::HeaderDef;
use crate::headerdef::HeaderDefMap;
use crate::key::{DcKey, Fingerprint, SignedPublicKey, SignedSecretKey};
use crate::keyring::Keyring;
use crate::log::LogExt;
use crate::peerstate::Peerstate;
use crate::pgp;
use crate::tools;

/// Tries to decrypt a message, but only if it is structured as an
/// Autocrypt message.
///
/// Returns decrypted body and a set of valid signature fingerprints
/// if successful.
///
/// If the message is wrongly signed, this will still return the decrypted
/// message but the HashSet will be empty.
pub async fn try_decrypt(
    context: &Context,
    mail: &ParsedMail<'_>,
    decryption_info: &DecryptionInfo,
) -> Result<Option<(Vec<u8>, HashSet<Fingerprint>)>> {
    // Possibly perform decryption
    let public_keyring_for_validate = keyring_from_peerstate(&decryption_info.peerstate);

    let encrypted_data_part = match get_autocrypt_mime(mail)
        .or_else(|| get_mixed_up_mime(mail))
        .or_else(|| get_attachment_mime(mail))
    {
        None => {
            // not an autocrypt mime message, abort and ignore
            return Ok(None);
        }
        Some(res) => res,
    };
    info!(context, "Detected Autocrypt-mime message");
    let private_keyring: Keyring<SignedSecretKey> = Keyring::new_self(context)
        .await
        .context("failed to get own keyring")?;

    decrypt_part(
        encrypted_data_part,
        private_keyring,
        public_keyring_for_validate,
    )
    .await
}

// TODO move somewhere else

#[derive(Debug)]
struct AuthenticationResults {
    dkim_passed: bool,
}

type AuthservId = String;

fn parse_authentication_results(
    context: &Context,
    headers: &mailparse::headers::Headers<'_>,
    from: &str,
) -> Result<HashMap<AuthservId, AuthenticationResults>> {
    // TODO old comment:
    // TODO this doesn't work for e.g. GMX which sells @gmx.de addresses, but uses gmx.net as its server
    // Config::ConfiguredProvider doesn't work for e.g. Gmail which uses mx.google.com.
    //
    // We could self-send a message during configure and use the Authentication-Results header from there -
    // this works for e.g. GMX, but not for Testrun and GMAIL.
    // -> Alternatively, we could send a message to nonexistent@example.com and wait for the NDN. This works
    //    for Gmail, but the Testrun NDN doesn't contain such a header, and GMX returns an error directly
    //    while sending.
    //
    // We could save this info in the provider db, but this only works for these providers.

    // let from = match from.first() {
    //     Some(f) => &f.addr,
    //     None => return Ok(HashMap::new()),
    // }; // TODO
    let sender_domain = crate::tools::EmailAddress::new(from)?.domain;

    let mut header_map: HashMap<AuthservId, Vec<String>> = HashMap::new();
    for header_value in headers.get_all_values(HeaderDef::AuthenticationResults.into()) {
        // TODO there could be a comment [CFWS] before the self domain. Do we care? Probably not.
        let authserv_id = header_value
            .split_whitespace()
            .next()
            .context("Empty header")?; // TODO do we really want to return Err here if it's empty
        header_map
            .entry(authserv_id.to_string())
            .or_default()
            .push(header_value);
    }

    let mut authresults_map = HashMap::new();
    for (authserv_id, headers) in header_map {
        let dkim_passed = authresults_dkim_passed(&headers, &sender_domain)?;
        authresults_map.insert(authserv_id, AuthenticationResults { dkim_passed });
    }

    Ok(authresults_map)
}

/// Parses the Authentication-Results headers belonging to a specific authserv-id
/// and returns whether they say that DKIM passed.
/// TODO document better
/// TODO if there are multiple headers and one says `pass`, one says `fail`, `none`
/// or whatever, then we still interpret that as `pass` - is this a problem?
fn authresults_dkim_passed(headers: &[String], sender_domain: &str) -> Result<bool> {
    for header_value in headers {
        if let Some((_start, dkim_to_end)) = header_value.split_once("dkim=") {
            let dkim_part = dkim_to_end
                .split(';')
                .next()
                .context("what the hell TODO")?;
            let dkim_parts: Vec<_> = dkim_part.split_whitespace().collect();
            if let Some(&"pass") = dkim_parts.first() {
                let header_d: &str = &format!("header.d={}", &sender_domain);
                let header_i: &str = &format!("header.i=@{}", &sender_domain);

                if dkim_parts.contains(&header_d) || dkim_parts.contains(&header_i) {
                    // We have found a `dkim=pass` header!
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

// TODO this is only half of the algorithm we thought of; we also wanted to save how sure we are
// about the authserv id. Like, a same-domain email is more trustworthy.
async fn update_authservid_candidates(
    context: &Context,
    authentication_results: &HashMap<AuthservId, AuthenticationResults>,
) -> Result<()> {
    let mut new_ids: HashSet<_> = authentication_results.keys().map(String::as_str).collect();
    if new_ids.is_empty() {
        // The incoming message doesn't contain any authentication results, maybe it's a
        // self-sent or a mailer-daemon message
        return Ok(());
    }

    let ids_config;
    if let Some(ids_config_temp) = context
        .get_config(crate::config::Config::AuthservIdCandidates)
        .await?
    {
        ids_config = ids_config_temp;
        let old_ids: HashSet<_> = ids_config.split(' ').collect();
        if !old_ids.is_empty() {
            new_ids = old_ids.intersection(&new_ids).copied().collect();
        }
    }
    // If there were no AuthservIdCandidates previously, just start with
    // the ones from the incoming email

    let new_config = new_ids.into_iter().collect::<Vec<_>>().join(" ");
    context
        .set_config(
            crate::config::Config::AuthservIdCandidates,
            Some(&new_config),
        )
        .await?;

    Ok(())
}

pub async fn create_decryption_info(
    context: &Context,
    mail: &ParsedMail<'_>,
    message_time: i64,
) -> Result<DecryptionInfo> {
    let from = mail
        .headers
        .get_header(HeaderDef::From_)
        .and_then(|from_addr| mailparse::addrparse_header(from_addr).ok())
        .and_then(|from| from.extract_single_info())
        .map(|from| from.addr)
        .unwrap_or_default();

    let autocrypt_header = Aheader::from_headers(&from, &mail.headers)
        .ok_or_log_msg(context, "Failed to parse Autocrypt header")
        .flatten();

    let authentication_results = parse_authentication_results(context, &mail.get_headers(), &from)?;
    update_authservid_candidates(context, &authentication_results).await?;
    // TODO code duplication with update_authservid_candidates()
    // TODO too much low-level code
    let mut dkim_passed = true; // TODO what do we want to do if there are multiple or no authservid candidates?
    if let Some(ids_config) = context
        .get_config(crate::config::Config::AuthservIdCandidates)
        .await?
    {
        let ids: HashSet<_> = ids_config.split(' ').collect();
        if let Some(authserv_id) = tools::single_value(ids) {
            // TODO unwrap
            dkim_passed = authentication_results.get(authserv_id).unwrap().dkim_passed;
        }
    }

    // TODO old comment Allow changes to the autocrypt key if DKIM passed.
    // If DKIM failed, we assume that the From address may have been forged
    // and therefore we prohibit changes to the autocrypt key.

    let peerstate = get_autocrypt_peerstate(
        context,
        &from,
        autocrypt_header.as_ref(),
        message_time,
        true, // TODO key changes should not be allowed if the sending domain sent DKIM-valid emails
              // until now, but this one is DKIM-invalid.
    )
    .await?;

    Ok(DecryptionInfo {
        from,
        autocrypt_header,
        peerstate,
        message_time,
    })
}

#[derive(Debug)]
pub struct DecryptionInfo {
    /// The From address. This is the address from the unnencrypted, outer
    /// From header.
    pub from: String,
    pub autocrypt_header: Option<Aheader>,
    /// The peerstate that will be used to validate the signatures
    pub peerstate: Option<Peerstate>,
    /// The timestamp when the message was sent.
    /// If this is older than the peerstate's last_seen, this probably
    /// means out-of-order message arrival, We don't modify the
    /// peerstate in this case.
    pub message_time: i64,
}

/// Returns a reference to the encrypted payload of a ["Mixed
/// Up"][pgpmime-message-mangling] message.
///
/// According to [RFC 3156] encrypted messages should have
/// `multipart/encrypted` MIME type and two parts, but Microsoft
/// Exchange and ProtonMail IMAP/SMTP Bridge are known to mangle this
/// structure by changing the type to `multipart/mixed` and prepending
/// an empty part at the start.
///
/// ProtonMail IMAP/SMTP Bridge prepends a part literally saying
/// "Empty Message", so we don't check its contents at all, checking
/// only for `text/plain` type.
///
/// Returns `None` if the message is not a "Mixed Up" message.
///
/// [RFC 3156]: https://www.rfc-editor.org/info/rfc3156
/// [pgpmime-message-mangling]: https://tools.ietf.org/id/draft-dkg-openpgp-pgpmime-message-mangling-00.html
fn get_mixed_up_mime<'a, 'b>(mail: &'a ParsedMail<'b>) -> Option<&'a ParsedMail<'b>> {
    if mail.ctype.mimetype != "multipart/mixed" {
        return None;
    }
    if let [first_part, second_part, third_part] = &mail.subparts[..] {
        if first_part.ctype.mimetype == "text/plain"
            && second_part.ctype.mimetype == "application/pgp-encrypted"
            && third_part.ctype.mimetype == "application/octet-stream"
        {
            Some(third_part)
        } else {
            None
        }
    } else {
        None
    }
}

/// Returns a reference to the encrypted payload of a message turned into attachment.
///
/// Google Workspace has an option "Append footer" which appends standard footer defined
/// by administrator to all outgoing messages. However, there is no plain text part in
/// encrypted messages sent by Delta Chat, so Google Workspace turns the message into
/// multipart/mixed MIME, where the first part is an empty plaintext part with a footer
/// and the second part is the original encrypted message.
fn get_attachment_mime<'a, 'b>(mail: &'a ParsedMail<'b>) -> Option<&'a ParsedMail<'b>> {
    if mail.ctype.mimetype != "multipart/mixed" {
        return None;
    }
    if let [first_part, second_part] = &mail.subparts[..] {
        if first_part.ctype.mimetype == "text/plain"
            && second_part.ctype.mimetype == "multipart/encrypted"
        {
            get_autocrypt_mime(second_part)
        } else {
            None
        }
    } else {
        None
    }
}

/// Returns a reference to the encrypted payload of a valid PGP/MIME message.
///
/// Returns `None` if the message is not a valid PGP/MIME message.
fn get_autocrypt_mime<'a, 'b>(mail: &'a ParsedMail<'b>) -> Option<&'a ParsedMail<'b>> {
    if mail.ctype.mimetype != "multipart/encrypted" {
        return None;
    }
    if let [first_part, second_part] = &mail.subparts[..] {
        if first_part.ctype.mimetype == "application/pgp-encrypted"
            && second_part.ctype.mimetype == "application/octet-stream"
        {
            Some(second_part)
        } else {
            None
        }
    } else {
        None
    }
}

/// Returns Ok(None) if nothing encrypted was found.
async fn decrypt_part(
    mail: &ParsedMail<'_>,
    private_keyring: Keyring<SignedSecretKey>,
    public_keyring_for_validate: Keyring<SignedPublicKey>,
) -> Result<Option<(Vec<u8>, HashSet<Fingerprint>)>> {
    let data = mail.get_body_raw()?;

    if has_decrypted_pgp_armor(&data) {
        let (plain, ret_valid_signatures) =
            pgp::pk_decrypt(data, private_keyring, &public_keyring_for_validate).await?;

        // Check for detached signatures.
        // If decrypted part is a multipart/signed, then there is a detached signature.
        let decrypted_part = mailparse::parse_mail(&plain)?;
        if let Some((content, valid_detached_signatures)) =
            validate_detached_signature(&decrypted_part, &public_keyring_for_validate)?
        {
            return Ok(Some((content, valid_detached_signatures)));
        } else {
            // If the message was wrongly or not signed, still return the plain text.
            // The caller has to check if the signatures set is empty then.

            return Ok(Some((plain, ret_valid_signatures)));
        }
    }

    Ok(None)
}

#[allow(clippy::indexing_slicing)]
fn has_decrypted_pgp_armor(input: &[u8]) -> bool {
    if let Some(index) = input.iter().position(|b| *b > b' ') {
        if input.len() - index > 26 {
            let start = index;
            let end = start + 27;

            return &input[start..end] == b"-----BEGIN PGP MESSAGE-----";
        }
    }

    false
}

/// Validates signatures of Multipart/Signed message part, as defined in RFC 1847.
///
/// Returns `None` if the part is not a Multipart/Signed part, otherwise retruns the set of key
/// fingerprints for which there is a valid signature.
fn validate_detached_signature(
    mail: &ParsedMail<'_>,
    public_keyring_for_validate: &Keyring<SignedPublicKey>,
) -> Result<Option<(Vec<u8>, HashSet<Fingerprint>)>> {
    if mail.ctype.mimetype != "multipart/signed" {
        return Ok(None);
    }

    if let [first_part, second_part] = &mail.subparts[..] {
        // First part is the content, second part is the signature.
        let content = first_part.raw_bytes;
        let signature = second_part.get_body_raw()?;
        let ret_valid_signatures =
            pgp::pk_validate(content, &signature, public_keyring_for_validate)?;

        Ok(Some((content.to_vec(), ret_valid_signatures)))
    } else {
        Ok(None)
    }
}

fn keyring_from_peerstate(peerstate: &Option<Peerstate>) -> Keyring<SignedPublicKey> {
    let mut public_keyring_for_validate: Keyring<SignedPublicKey> = Keyring::new();
    if let Some(ref peerstate) = *peerstate {
        if let Some(key) = &peerstate.public_key {
            public_keyring_for_validate.add(key.clone());
        } else if let Some(key) = &peerstate.gossip_key {
            public_keyring_for_validate.add(key.clone());
        }
    }
    public_keyring_for_validate
}

/// Applies Autocrypt header to Autocrypt peer state and saves it into the database.
///
/// If we already know this fingerprint from another contact's peerstate, return that
/// peerstate in order to make AEAP work, but don't save it into the db yet.
///
/// Returns updated peerstate.
pub(crate) async fn get_autocrypt_peerstate(
    context: &Context,
    from: &str,
    autocrypt_header: Option<&Aheader>,
    message_time: i64,
    allow_change: bool,
) -> Result<Option<Peerstate>> {
    let mut peerstate;

    // Apply Autocrypt header
    if let Some(header) = autocrypt_header {
        // The "from_verified_fingerprint" part is for AEAP:
        // If we know this fingerprint from another addr,
        // we may want to do a transition from this other addr
        // (and keep its peerstate)
        // For security reasons, for now, we only do a transition
        // if the fingerprint is verified.
        peerstate = Peerstate::from_verified_fingerprint_or_addr(
            context,
            &header.public_key.fingerprint(),
            from,
        )
        .await?;

        if let Some(ref mut peerstate) = peerstate {
            if addr_cmp(&peerstate.addr, from) && allow_change {
                peerstate.apply_header(header, message_time);
                peerstate.save_to_db(&context.sql, false).await?;
            }
            // If `peerstate.addr` and `from` differ, this means that
            // someone is using the same key but a different addr, probably
            // because they made an AEAP transition.
            // But we don't know if that's legit until we checked the
            // signatures, so wait until then with writing anything
            // to the database.
        } else {
            let p = Peerstate::from_header(header, message_time);
            p.save_to_db(&context.sql, true).await?;
            peerstate = Some(p);
        }
    } else {
        peerstate = Peerstate::from_addr(context, from).await?;
    }

    Ok(peerstate)
}

#[cfg(test)]
mod tests {
    use crate::receive_imf::receive_imf;
    use crate::test_utils::TestContext;

    use super::*;

    #[test]
    fn test_has_decrypted_pgp_armor() {
        let data = b" -----BEGIN PGP MESSAGE-----";
        assert_eq!(has_decrypted_pgp_armor(data), true);

        let data = b"    \n-----BEGIN PGP MESSAGE-----";
        assert_eq!(has_decrypted_pgp_armor(data), true);

        let data = b"    -----BEGIN PGP MESSAGE---";
        assert_eq!(has_decrypted_pgp_armor(data), false);

        let data = b" -----BEGIN PGP MESSAGE-----";
        assert_eq!(has_decrypted_pgp_armor(data), true);

        let data = b"blas";
        assert_eq!(has_decrypted_pgp_armor(data), false);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_mixed_up_mime() -> Result<()> {
        // "Mixed Up" mail as received when sending an encrypted
        // message using Delta Chat Desktop via ProtonMail IMAP/SMTP
        // Bridge.
        let mixed_up_mime = include_bytes!("../test-data/message/protonmail-mixed-up.eml");
        let mail = mailparse::parse_mail(mixed_up_mime)?;
        assert!(get_autocrypt_mime(&mail).is_none());
        assert!(get_mixed_up_mime(&mail).is_some());
        assert!(get_attachment_mime(&mail).is_none());

        // Same "Mixed Up" mail repaired by Thunderbird 78.9.0.
        //
        // It added `X-Enigmail-Info: Fixed broken PGP/MIME message`
        // header although the repairing is done by the built-in
        // OpenPGP support, not Enigmail.
        let repaired_mime = include_bytes!("../test-data/message/protonmail-repaired.eml");
        let mail = mailparse::parse_mail(repaired_mime)?;
        assert!(get_autocrypt_mime(&mail).is_some());
        assert!(get_mixed_up_mime(&mail).is_none());
        assert!(get_attachment_mime(&mail).is_none());

        // Another form of "Mixed Up" mail created by Google Workspace,
        // where original message is turned into attachment to empty plaintext message.
        let attachment_mime = include_bytes!("../test-data/message/google-workspace-mixed-up.eml");
        let mail = mailparse::parse_mail(attachment_mime)?;
        assert!(get_autocrypt_mime(&mail).is_none());
        assert!(get_mixed_up_mime(&mail).is_none());
        assert!(get_attachment_mime(&mail).is_some());

        let bob = TestContext::new_bob().await;
        receive_imf(&bob, attachment_mime, false).await?;
        let msg = bob.get_last_msg().await;
        assert_eq!(msg.text.as_deref(), Some("Hello from Thunderbird!"));

        Ok(())
    }
}
