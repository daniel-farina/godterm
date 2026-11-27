use super::*;

fn tabs_of(n: usize) -> Tabs {
    let mut t = Tabs::default();
    for i in 0..n {
        let id = t.add(&format!("T{i}"), &format!("S{i}"), "about:blank");
        if t.active == 0 {
            t.active = id;
        }
    }
    t
}

fn ids(t: &Tabs) -> Vec<u32> {
    t.pages.iter().map(|p| p.id).collect()
}

#[test]
fn pages_get_ids_that_are_never_reused_even_across_owners() {
    let mut t = tabs_of(3);
    let first = ids(&t);
    assert!(first[0] > 0 && first[0] < first[1] && first[1] < first[2]);
    let mut other = tabs_of(2);
    assert!(
        ids(&other).iter().all(|i| !first.contains(i)),
        "two owners never share a page id"
    );
    t.remove_target("T1");
    let id = t.add("T9", "S9", "about:blank");
    assert!(id > first[2] && !ids(&other).contains(&id));
    assert!(t.get(first[1]).is_none());
    let late = other.add_asleep("about:blank");
    assert!(late > id);
}

#[test]
fn six_pages_at_most_counting_the_ones_still_joining_and_the_sleeping() {
    let mut t = tabs_of(3);
    t.add_asleep("http://localhost:1/kept");
    assert!(t.begin("A"));
    assert!(t.begin("B"));
    assert!(!t.can_add());
    assert!(!t.begin("C"), "the seventh is refused");
    assert!(!t.begin("A"), "a known target is not joined twice");
    t.abandon("B");
    assert!(t.can_add());
    let id = t.add("A", "SA", "http://localhost:1/");
    assert_eq!(t.pages.len(), 5);
    assert!(t.joining.is_empty());
    assert_eq!(t.get(id).unwrap().url, "http://localhost:1/");
}

#[test]
fn closing_the_active_page_picks_its_left_neighbour_then_the_next() {
    let mut t = tabs_of(3);
    let i = ids(&t);
    t.active = i[1];
    let r = t.remove_target("T1").unwrap();
    assert!(r.was_active && !r.empty);
    assert_eq!(t.active, i[0]);
    let r = t.remove_target("T0").unwrap();
    assert!(r.was_active && !r.empty);
    assert_eq!(t.active, i[2]);
}

#[test]
fn closing_another_page_keeps_the_active_one() {
    let mut t = tabs_of(3);
    let i = ids(&t);
    t.active = i[2];
    let r = t.remove_target("T0").unwrap();
    assert!(!r.was_active);
    assert_eq!(t.active, i[2]);
    assert!(t.remove_target("T0").is_none(), "closing twice is harmless");
    assert!(t.remove_id(i[0]).is_none());
    assert!(t.remove_id(i[1]).is_some());
}

#[test]
fn the_last_page_going_is_reported_so_a_blank_one_can_replace_it() {
    let mut t = tabs_of(1);
    let r = t.remove_target("T0").unwrap();
    assert!(r.was_active && r.empty);
    assert_eq!(t.active, 0);
    assert!(t.cur().is_none());
}

#[test]
fn pages_are_found_by_session_and_target_and_listed_in_order() {
    let mut t = tabs_of(2);
    t.by_sid("S1").unwrap().title = "Two".into();
    t.by_target("T0").unwrap().url = "http://localhost:3000/".into();
    let l = t.list();
    assert_eq!(l[0]["url"], "http://localhost:3000/");
    assert_eq!(l[0]["active"], json!(true));
    assert_eq!(l[1]["title"], "Two");
    assert_eq!(l[1]["active"], json!(false));
    assert!(l[0].get("asleep").is_none());
    assert!(t.by_sid("nope").is_none());
}

#[test]
fn console_and_network_belong_to_their_page() {
    let mut t = tabs_of(2);
    let first = ids(&t)[0];
    t.by_sid("S0").unwrap().console.push_back(Entry {
        seq: 1,
        kind: "log".into(),
        text: "first".into(),
        url: String::new(),
        status: 0,
    });
    assert!(t.by_sid("S1").unwrap().console.is_empty());
    assert_eq!(t.get(first).unwrap().console.len(), 1);
}

#[test]
fn a_sleeping_page_keeps_its_id_place_and_facts_and_wakes_as_itself() {
    let mut t = tabs_of(3);
    let i = ids(&t);
    {
        let p = t.get_mut(i[1]).unwrap();
        p.url = "http://localhost:3000/docs".into();
        p.title = "Docs".into();
        p.scroll = (0.0, 420.0);
        log_to(p, "log", "kept".into(), String::new(), 0, false);
    }
    let (target, sid) = t.rest(i[1]).unwrap();
    assert_eq!((target.as_str(), sid.as_str()), ("T1", "S1"));
    assert!(
        t.rest(i[1]).is_none(),
        "a sleeping page is not put to sleep twice"
    );
    assert_eq!(ids(&t), i, "it keeps its place in the strip");
    let l = t.list();
    assert_eq!(l[1]["asleep"], json!(true));
    assert_eq!(l[1]["url"], "http://localhost:3000/docs");
    assert_eq!(l[1]["title"], "Docs");
    assert_eq!(l[1]["id"], json!(i[1]));
    assert_eq!(t.asleep(), 1);
    assert!(t.by_sid("S1").is_none() && t.by_sid("").is_none());
    assert!(t.by_target("").is_none() && t.remove_target("").is_none());
    assert!(!t.known(""));
    assert!(!t.revive(i[0], "X", "Y"), "an awake page is not revived");
    assert!(t.revive(i[1], "T7", "S7"));
    let p = t.get(i[1]).unwrap();
    assert!(!p.asleep);
    assert_eq!((p.target.as_str(), p.sid.as_str()), ("T7", "S7"));
    assert_eq!(p.scroll, (0.0, 420.0));
    assert_eq!(p.console.len(), 1, "its console is kept across the sleep");
    assert_eq!(t.asleep(), 0);
    assert!(!t.revive(9_999_999, "T8", "S8"));
}

#[test]
fn the_book_says_whose_page_it_is_and_refuses_everyone_else() {
    let mut k = Book::default();
    k.tag("TA", "SA", 1);
    k.tag("TB", "SB", 2);
    assert_eq!(k.owner_of_target("TA"), Some(1));
    assert_eq!(k.by_sid("SB"), Some((2, "TB".to_string())));
    assert!(k.owns(1, &Key::Sid("SA")).is_ok());
    assert!(k.owns(1, &Key::Target("TA")).is_ok());
    for key in [
        Key::Sid("SB"),
        Key::Target("TB"),
        Key::Sid(""),
        Key::Target(""),
        Key::Sid("nope"),
        Key::Target("TA2"),
    ] {
        assert_eq!(k.owns(1, &key).unwrap_err(), NOT_OWNED);
    }
    assert!(k.owns(2, &Key::Sid("SA")).is_err());
    assert!(k.owns(0, &Key::Target("TA")).is_err());
    assert_eq!((k.owned(1), k.owned(2), k.live()), (1, 1, 2));
    k.reserved = 1;
    assert_eq!(k.live(), 3, "a page being opened already counts");
    k.forget("TA");
    assert!(k.owns(1, &Key::Sid("SA")).is_err());
    assert!(k.owns(1, &Key::Target("TA")).is_err());
    assert_eq!(k.live(), 2);
}

#[test]
fn the_limits_are_clamped() {
    let _g = crate::config::testing::LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    set_page_cap(1);
    assert_eq!(page_cap(), MAX_PAGES);
    set_page_cap(100_000);
    assert_eq!(page_cap(), 200);
    set_page_cap(MAX_TOTAL);
    assert_eq!(page_cap(), 40);
    set_sleep_after(Duration::ZERO);
    assert!(sleep_after().is_none(), "0 never sleeps");
    set_sleep_after(Duration::from_secs(SLEEP_MINUTES * 60));
    assert_eq!(sleep_after(), Some(Duration::from_secs(1800)));
}

#[test]
fn every_page_gets_the_same_address_policy() {
    for ok in [
        "",
        "about:blank",
        "http://localhost:3000/x",
        "https://example.com",
        "chrome-error://chromewebdata/",
    ] {
        assert!(url_allowed(ok), "{ok}");
    }
    for bad in [
        "file:///etc/hosts",
        "chrome://settings",
        "javascript:alert(1)",
        "data:text/html,hi",
        "blob:http://localhost/x",
        "view-source:http://localhost/",
        "ftp://example.com/",
    ] {
        assert!(!url_allowed(bad), "{bad}");
    }
    assert!(check_url("file:///etc/hosts").is_err());
    assert!(check_url("javascript:alert(1)").is_err());
    assert_eq!(MAX_PAGES, 6);
}
