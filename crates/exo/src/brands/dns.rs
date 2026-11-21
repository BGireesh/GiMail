//! TXT lookups through macOS's own resolver (`dns_sd`, the system's DNS settings, VPNs and their
//! split DNS included), with their records' TTL.

use std::time::Duration;

/// How long a lookup waits for its answer.
const TIMEOUT: Duration = Duration::from_secs(5);

/// A name's TXT records, each one's strings joined, and how long they may be kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Txt {
    pub(crate) records: Vec<String>,
    pub(crate) ttl: Duration,
}

/// Looks up `name`'s TXT records, off the async runtime. `Ok(None)` when it has none, `Err` when
/// the resolver didn't answer.
pub(crate) async fn txt(name: &str) -> Result<Option<Txt>, ()> {
    let name = name.to_owned();
    tokio::task::spawn_blocking(move || lookup(&name))
        .await
        .map_err(|_| ())?
}

/// The TXT record's character-strings, each a length byte then that many bytes, joined.
fn strings(rdata: &[u8]) -> String {
    let (mut text, mut rest) = (Vec::new(), rdata);
    while let Some((&length, tail)) = rest.split_first() {
        let length = usize::from(length).min(tail.len());
        text.extend_from_slice(&tail[..length]);
        rest = &tail[length..];
    }
    String::from_utf8_lossy(&text).into_owned()
}

#[cfg(target_os = "macos")]
fn lookup(name: &str) -> Result<Option<Txt>, ()> {
    use std::ffi::{CString, c_char, c_int, c_void};

    type Ref = *mut c_void;
    type Reply = extern "C" fn(
        Ref,
        u32,
        u32,
        i32,
        *const c_char,
        u16,
        u16,
        u16,
        *const c_void,
        u32,
        *mut c_void,
    );
    unsafe extern "C" {
        fn DNSServiceQueryRecord(
            reference: *mut Ref,
            flags: u32,
            interface: u32,
            name: *const c_char,
            kind: u16,
            class: u16,
            reply: Reply,
            context: *mut c_void,
        ) -> i32;
        fn DNSServiceProcessResult(reference: Ref) -> i32;
        fn DNSServiceRefSockFD(reference: Ref) -> c_int;
        fn DNSServiceRefDeallocate(reference: Ref);
    }
    /// `kDNSServiceFlagsMoreComing`, and `kDNSServiceFlagsReturnIntermediates`, which brings
    /// "no such record" back as an answer instead of waiting on.
    const MORE_COMING: u32 = 0x1;
    const RETURN_INTERMEDIATES: u32 = 0x1000;
    const TXT: u16 = 16;
    const IN: u16 = 1;
    /// `kDNSServiceErr_NoSuchName` and `kDNSServiceErr_NoSuchRecord`.
    const NO_SUCH_NAME: i32 = -65538;
    const NO_SUCH_RECORD: i32 = -65554;

    /// What the callback was told.
    #[derive(Default)]
    struct Answers {
        records: Vec<String>,
        ttl: Option<u32>,
        error: Option<i32>,
        done: bool,
    }
    extern "C" fn reply(
        _: Ref,
        flags: u32,
        _: u32,
        error: i32,
        _: *const c_char,
        kind: u16,
        _: u16,
        length: u16,
        rdata: *const c_void,
        ttl: u32,
        context: *mut c_void,
    ) {
        // SAFETY: `context` is the `Answers` the lookup below passed, alive for its whole call, and
        // the callback only runs inside its `DNSServiceProcessResult`.
        let answers = unsafe { &mut *context.cast::<Answers>() };
        if error != 0 {
            answers.error = Some(error);
        } else if kind == TXT && !rdata.is_null() {
            // Intermediate answers include the CNAMEs on the way to the TXT records, which aren't
            // text; the lookup waits on for the records themselves.
            // SAFETY: dns_sd hands `length` bytes of record data at `rdata`, valid in the callback.
            let rdata = unsafe { std::slice::from_raw_parts(rdata.cast::<u8>(), length.into()) };
            answers.records.push(strings(rdata));
            answers.ttl = Some(answers.ttl.map_or(ttl, |shortest| shortest.min(ttl)));
        }
        answers.done = flags & MORE_COMING == 0;
    }

    let name = CString::new(name).map_err(|_| ())?;
    let mut answers = Answers::default();
    let mut reference: Ref = std::ptr::null_mut();
    // SAFETY: the name is NUL-terminated, the reference is written before it's used, and
    // `answers` outlives the reference, which is deallocated before returning.
    let started = unsafe {
        DNSServiceQueryRecord(
            &raw mut reference,
            RETURN_INTERMEDIATES,
            0,
            name.as_ptr(),
            TXT,
            IN,
            reply,
            (&raw mut answers).cast(),
        )
    };
    if started != 0 {
        log::debug!("looking up TXT records: dns_sd error {started}");
        return Err(());
    }
    let deadline = std::time::Instant::now() + TIMEOUT;
    let result = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break Err(());
        }
        // SAFETY: the reference is live.
        let mut poll = libc::pollfd {
            fd: unsafe { DNSServiceRefSockFD(reference) },
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd, as said.
        let ready = unsafe { libc::poll(&raw mut poll, 1, left.as_millis().min(5000) as c_int) };
        if ready < 0 {
            break Err(());
        }
        if ready == 0 {
            continue;
        }
        // SAFETY: the reference is live, and the callback writes only `answers`.
        if unsafe { DNSServiceProcessResult(reference) } != 0 {
            break Err(());
        }
        match answers.error {
            Some(NO_SUCH_NAME | NO_SUCH_RECORD) => break Ok(None),
            Some(_) => break Err(()),
            None if answers.done && !answers.records.is_empty() => {
                break Ok(Some(Txt {
                    records: std::mem::take(&mut answers.records),
                    ttl: Duration::from_secs(answers.ttl.unwrap_or_default().into()),
                }));
            }
            None => {}
        }
    };
    // SAFETY: the reference was made above and isn't used after this.
    unsafe { DNSServiceRefDeallocate(reference) };
    result
}

/// Off macOS there's no system resolver to ask, so no brand is looked up.
#[cfg(not(target_os = "macos"))]
fn lookup(_: &str) -> Result<Option<Txt>, ()> {
    Err(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_records_strings_are_joined() {
        let rdata = b"\x0bv=BIMI1; l=\x05https";
        assert_eq!(super::strings(rdata), "v=BIMI1; l=https");
    }
}
