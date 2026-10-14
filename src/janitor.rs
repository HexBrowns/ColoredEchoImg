//! 見回り: 呼ばれなくなった履歴を時間だけで捨てるスレッド（1 本）。TimeDisplace_H v1.0.4 の `janitor.rs` と同じ作り。
//!
//! `StateStore::purge_stale` は、どれかのオブジェクトが描かれたときにしか走らない。オブジェクトを全部消すと誰も呼ばないので、
//! 持っている履歴（全部で最大 2 GiB）が本体を閉じるまで残っていた。ここで `CHECK_INTERVAL` ごとに `StateStore::purge_idle` を呼ぶ。
//!
//! - 本体の API は呼ばない（スクリプトモジュールの状態に触るだけ）
//! - モジュールが外れる前に止めて `join` する（`Drop`。`.claude/rules/au2-rs-plugin.md`「裏のスレッドは、プラグインが外れる前に止めて終わりを待つ」）。
//!   待つのは `Condvar` なので、止める印を立てればすぐ起きる

use crate::store::StateStore;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// 見回りの間隔。捨てる条件（`store::STALE_AFTER` = 10 秒呼ばれない）に対して、最大 1 秒遅れで捨てる
pub const CHECK_INTERVAL: Duration = Duration::from_secs(1);

pub struct Janitor {
    stop: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl Janitor {
    pub fn start(store: Arc<Mutex<StateStore>>) -> Self {
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let flag = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("ColoredEchoImg janitor".into())
            .spawn(move || run(&flag, &store, CHECK_INTERVAL))
            .ok();
        if handle.is_none() {
            aviutl2::lprintln!(warn, "ColoredEchoImg: 見回りのスレッドを起こせませんでした（使わなくなった履歴は、別の色付き残像が描かれたときに捨てます）");
        }
        Self { stop, handle }
    }
}

impl Drop for Janitor {
    fn drop(&mut self) {
        let (lock, cv) = &*self.stop;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_all();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run(stop: &(Mutex<bool>, Condvar), store: &Mutex<StateStore>, interval: Duration) {
    let (lock, cv) = stop;
    let mut stopped = lock.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if *stopped {
            return;
        }
        let (g, _) = cv.wait_timeout(stopped, interval).unwrap_or_else(|e| e.into_inner());
        stopped = g;
        if *stopped {
            return;
        }
        // 止める印の Mutex を離してから状態の Mutex を取る（Drop が印を立てるのを待たせない）
        drop(stopped);
        // 毒されていたら触らない（lib.rs の関数も、毒された状態ではエラーを返して何もしない）
        let purged = store.lock().ok().map(|mut s| s.purge_idle(Instant::now()));
        if let Some((n, bytes)) = purged {
            if n > 0 {
                aviutl2::lprintln!(
                    info,
                    "ColoredEchoImg: 使われていない履歴 {n} 個（{:.1} MB）を捨てました",
                    bytes as f64 / (1024.0 * 1024.0)
                );
            }
        }
        stopped = lock.lock().unwrap_or_else(|e| e.into_inner());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trail::test_params;

    /// 止める印を立てると、間隔の途中でもすぐ終わる
    #[test]
    fn drop_stops_quickly() {
        let store = Arc::new(Mutex::new(StateStore::new()));
        let j = Janitor::start(Arc::clone(&store));
        std::thread::sleep(Duration::from_millis(50));
        let t = Instant::now();
        drop(j);
        assert!(t.elapsed() < Duration::from_millis(500), "{:?}", t.elapsed());
    }

    /// 呼ばれなくなった履歴を、誰も呼ばなくても捨てる
    #[test]
    fn purges_without_calls() {
        let store = Arc::new(Mutex::new(StateStore::with_limits(1 << 30, 300, Duration::from_millis(100))));
        for f in 0..3 {
            let mut cur = vec![9u8, 0, 0, 255, 0, 0, 0, 0];
            store.lock().unwrap().process("gone", &mut cur, 2, 1, &test_params(f, 12, 1.0), Instant::now());
        }
        assert!(store.lock().unwrap().total_bytes() > 0);
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let (flag, st) = (Arc::clone(&stop), Arc::clone(&store));
        let h = std::thread::spawn(move || run(&flag, &st, Duration::from_millis(20)));
        let t = Instant::now();
        while store.lock().unwrap().total_bytes() > 0 && t.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(10));
        }
        *stop.0.lock().unwrap() = true;
        stop.1.notify_all();
        h.join().unwrap();
        assert_eq!(store.lock().unwrap().total_bytes(), 0);
    }
}
