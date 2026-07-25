//! Sessions carried by a signed cookie.

use churust_core::{Churust, Session, Sessions, TestClient};

fn app() -> churust_core::App {
    Churust::server()
        .install(Sessions::cookie("test-key"))
        .routing(|r| {
            r.get("/bump", |s: Session| async move {
                let n: u32 = s.get("count").and_then(|v| v.parse().ok()).unwrap_or(0);
                s.set("count", (n + 1).to_string());
                format!("{}", n + 1)
            });
            r.get("/peek", |s: Session| async move {
                s.get("count").unwrap_or_else(|| "none".into())
            });
            r.get("/logout", |s: Session| async move {
                s.clear();
                "bye".to_string()
            });
        })
        .build()
}

/// Pull the session cookie value out of a Set-Cookie header.
fn session_cookie(res: &churust_core::TestResponse) -> String {
    let sc = res.header("set-cookie").expect("expected a session cookie");
    sc.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn a_session_persists_across_requests() {
    let client = TestClient::new(app());

    let first = client.get("/bump").send().await;
    assert_eq!(first.text(), "1");
    let cookie = session_cookie(&first);

    let second = client.get("/bump").header("cookie", &cookie).send().await;
    assert_eq!(second.text(), "2", "the session should have carried over");
}

#[tokio::test]
async fn no_cookie_means_a_fresh_session() {
    let client = TestClient::new(app());
    assert_eq!(client.get("/bump").send().await.text(), "1");
    assert_eq!(
        client.get("/bump").send().await.text(),
        "1",
        "without the cookie each request starts over"
    );
}

#[tokio::test]
async fn an_unchanged_session_is_not_reissued() {
    let client = TestClient::new(app());
    let first = client.get("/bump").send().await;
    let cookie = session_cookie(&first);

    // /peek only reads.
    let res = client.get("/peek").header("cookie", &cookie).send().await;
    assert_eq!(res.text(), "1");
    assert!(
        res.header("set-cookie").is_none(),
        "an unchanged session must not be rewritten, which would also extend its expiry"
    );
}

#[tokio::test]
async fn a_tampered_cookie_is_rejected() {
    let client = TestClient::new(app());
    let first = client.get("/bump").send().await;
    let cookie = session_cookie(&first);

    // Flip the payload while keeping the signature.
    let tampered = cookie.replace("count=1", "count=99");
    assert_ne!(
        tampered, cookie,
        "the test needs to actually change something"
    );

    let res = client.get("/peek").header("cookie", &tampered).send().await;
    assert_eq!(
        res.text(),
        "none",
        "a payload that does not match its signature must be discarded, not trusted"
    );
}

#[tokio::test]
async fn a_cookie_signed_with_another_key_is_rejected() {
    let other = Churust::server()
        .install(Sessions::cookie("a-different-key"))
        .routing(|r| {
            r.get("/bump", |s: Session| async move {
                s.set("count", "42");
                "set".to_string()
            });
        })
        .build();

    let foreign = session_cookie(&TestClient::new(other).get("/bump").send().await);

    let res = TestClient::new(app())
        .get("/peek")
        .header("cookie", &foreign)
        .send()
        .await;
    assert_eq!(
        res.text(),
        "none",
        "another key's cookie must not be honoured"
    );
}

#[tokio::test]
async fn clear_empties_the_session() {
    let client = TestClient::new(app());
    let cookie = session_cookie(&client.get("/bump").send().await);

    let out = client.get("/logout").header("cookie", &cookie).send().await;
    let cleared = session_cookie(&out);

    let res = client.get("/peek").header("cookie", &cleared).send().await;
    assert_eq!(res.text(), "none");
}

#[tokio::test]
async fn values_containing_separators_survive() {
    let app = Churust::server()
        .install(Sessions::cookie("k"))
        .routing(|r| {
            r.get("/set", |s: Session| async move {
                s.set("odd", "a=b&c.d%e");
                "ok".to_string()
            });
            r.get("/get", |s: Session| async move {
                s.get("odd").unwrap_or_else(|| "none".into())
            });
        })
        .build();
    let client = TestClient::new(app);

    let cookie = session_cookie(&client.get("/set").send().await);
    let res = client.get("/get").header("cookie", &cookie).send().await;
    assert_eq!(res.text(), "a=b&c.d%e");
}

#[tokio::test]
async fn the_signature_is_a_real_mac() {
    // HMAC-SHA256 is 32 bytes, hex-encoded to 64 characters. The previous FNV
    // digest was 16, which is why this asserts the width: a regression to a
    // short non-cryptographic tag would be silent otherwise.
    let client = TestClient::new(app());
    let cookie = session_cookie(&client.get("/bump").send().await);
    let value = cookie.split_once('=').unwrap().1;
    let sig = value.split_once('.').unwrap().0;

    assert_eq!(sig.len(), 64, "expected a hex HMAC-SHA256 tag, got {sig:?}");
    assert!(sig.chars().all(|c| c.is_ascii_hexdigit()), "got {sig:?}");
}

#[tokio::test]
async fn flipping_one_bit_of_the_signature_is_rejected() {
    let client = TestClient::new(app());
    let cookie = session_cookie(&client.get("/bump").send().await);

    let (name, value) = cookie.split_once('=').unwrap();
    let (sig, payload) = value.split_once('.').unwrap();
    let mut chars: Vec<char> = sig.chars().collect();
    chars[0] = if chars[0] == 'a' { 'b' } else { 'a' };
    let forged = format!("{name}={}.{payload}", chars.into_iter().collect::<String>());

    let res = client.get("/peek").header("cookie", &forged).send().await;
    assert_eq!(res.text(), "none");
}

mod expiry {
    use churust_core::session::{CookieStore, SessionStore};
    use std::collections::BTreeMap;

    fn data() -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("user".into(), "ana".into());
        m
    }

    #[test]
    fn a_cookie_past_its_signed_deadline_is_refused() {
        // `Max-Age` on `Set-Cookie` is a hint to a well-behaved client; a copied
        // cookie ignores it. Without a deadline inside the signature, a captured
        // session stayed valid for as long as the signing key did.
        let issuer = CookieStore::new(b"k".to_vec()).with_max_age(-1);
        let raw = issuer.store(&data()).unwrap();
        let reader = CookieStore::new(b"k".to_vec()).with_max_age(3600);
        assert!(
            reader.load(&raw).is_none(),
            "an expired session cookie was accepted"
        );
    }

    #[test]
    fn a_cookie_inside_its_deadline_still_loads() {
        let store = CookieStore::new(b"k".to_vec()).with_max_age(3600);
        let raw = store.store(&data()).unwrap();
        let back = store.load(&raw).expect("a live session must load");
        assert_eq!(back.get("user").map(String::as_str), Some("ana"));
    }

    #[test]
    fn the_deadline_is_not_visible_as_session_data() {
        // The reserved key is an implementation detail, not something a handler
        // should see in its session map.
        let store = CookieStore::new(b"k".to_vec()).with_max_age(3600);
        let raw = store.store(&data()).unwrap();
        let back = store.load(&raw).unwrap();
        assert_eq!(back.len(), 1, "{back:?}");
        assert!(back.keys().all(|k| !k.starts_with("__churust")), "{back:?}");
    }

    #[test]
    fn an_application_cannot_forge_the_deadline_key() {
        // Writing the reserved key from application code must not become the
        // signed deadline.
        let store = CookieStore::new(b"k".to_vec()).with_max_age(3600);
        let mut m = data();
        m.insert("__churust_exp".into(), "99999999999".into());
        let raw = store.store(&m).unwrap();
        let back = store.load(&raw).unwrap();
        assert_eq!(back.len(), 1, "the forged key survived: {back:?}");
    }

    #[test]
    fn without_a_max_age_a_cookie_does_not_expire() {
        // Unchanged behaviour when no deadline was asked for.
        let store = CookieStore::new(b"k".to_vec());
        let raw = store.store(&data()).unwrap();
        assert!(store.load(&raw).is_some());
    }

    #[test]
    fn the_deadline_is_covered_by_the_signature() {
        // Editing the expiry must invalidate the cookie rather than extend it.
        let store = CookieStore::new(b"k".to_vec()).with_max_age(-1);
        let raw = store.store(&data()).unwrap();
        let (sig, payload) = raw.split_once('.').unwrap();
        let tampered = format!(
            "{sig}.{}",
            payload.replace("__churust_exp=", "__churust_exp=9")
        );
        assert!(store.load(&tampered).is_none());
    }
}
