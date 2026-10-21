//! A transfer that starts on the direct link and loses it half way finishes over the
//! companion channel, intact: wired the way the client and companion are (the source's
//! pump and the sink's fetches and acks prefer the direct link while it's up).

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rdp_companion_proto::Entry;
use rdp_companion_proto::Message;
use rdp_companion_proto::transfer::{Inflow, Outbox, Produced, Pump, Source};
use rdp_direct::{DirectSlot, Peer, Prepared};

/// Bytes that don't compress.
fn noise(len: usize) -> Vec<u8> {
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// Stands in for the RDP channel: delivers in order, a little late.
fn channel(deliver: impl Fn(Message) + Send + 'static) -> mpsc::Sender<Message> {
    let (tx, rx) = mpsc::channel::<Message>();
    std::thread::spawn(move || {
        for message in rx {
            std::thread::sleep(Duration::from_micros(200));
            deliver(message);
        }
    });
    tx
}

#[test]
fn a_transfer_survives_losing_the_direct_link() {
    const LEN: usize = 32 << 20;
    let data = Arc::new(noise(LEN));
    let entries: Arc<[Entry]> = vec![Entry {
        path: "big.bin".into(),
        dir: false,
        size: LEN as u64,
        modified: 0,
    }]
    .into();

    // The source side (say, the client sending a file).
    let source_slot = DirectSlot::default();
    let pump_cell: Arc<Mutex<Option<Arc<Pump>>>> = Arc::default();
    // The sink side (the companion receiving it).
    let sink_slot = DirectSlot::default();
    let inflow_cell: Arc<Mutex<Option<Arc<Inflow>>>> = Arc::default();

    let to_source_pump = {
        let pump = Arc::clone(&pump_cell);
        move |message: Message| {
            let pump = pump.lock().unwrap().clone().unwrap();
            if let Message::Close { id, error } = message {
                pump.closed(id, error);
            } else {
                pump.receive(&message);
            }
        }
    };
    let to_sink = {
        let inflow = Arc::clone(&inflow_cell);
        move |message: Message| {
            if let Some(inflow) = inflow.lock().unwrap().clone() {
                inflow.receive(message);
            }
        }
    };
    // Data that came the slow way (after the link was cut).
    let fallback_data = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let channel_to_sink = Mutex::new(channel({
        let to_sink = to_sink.clone();
        let fallback_data = Arc::clone(&fallback_data);
        move |message: Message| {
            if matches!(message, Message::Data { .. }) {
                fallback_data.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            to_sink(message);
        }
    }));
    let channel_to_source = Mutex::new(channel(to_source_pump.clone()));

    let route = source_slot.clone();
    let pump = Arc::new(
        Pump::spawn(
            "failover-pump",
            move |message| route.try_send(&message) || channel_to_sink.lock().unwrap().send(message).is_ok(),
            |_| {},
        )
        .unwrap(),
    );
    *pump_cell.lock().unwrap() = Some(Arc::clone(&pump));
    pump.start(Source::new(
        2,
        Arc::clone(&entries),
        Box::new(Produced::new({
            let data = Arc::clone(&data);
            move |_| Ok(data.to_vec())
        })),
    ));

    let sink_route = sink_slot.clone();
    let outbox: Outbox = Arc::new(move |message| {
        if !sink_route.try_send(&message) {
            let _ = channel_to_source.lock().unwrap().send(message);
        }
    });
    let inflow = Arc::new(Inflow::new(2, entries, outbox));
    *inflow_cell.lock().unwrap() = Some(Arc::clone(&inflow));

    // The direct link between them.
    let a = Prepared::loopback().unwrap();
    let b = Prepared::loopback().unwrap();
    let (to_b, to_a) = (
        Peer {
            fingerprint: b.fingerprint(),
            candidates: b.candidates().to_vec(),
        },
        Peer {
            fingerprint: a.fingerprint(),
            candidates: a.candidates().to_vec(),
        },
    );
    let accepted = {
        let on_message = Box::new(to_sink.clone());
        let (slot, inflow) = (sink_slot.clone(), Arc::clone(&inflow));
        let on_closed = Box::new(move |_| {
            slot.set(None);
            inflow.resume();
        });
        std::thread::spawn(move || rdp_direct::accept(a, to_b, Duration::from_secs(10), on_message, on_closed))
    };
    let connected = {
        let (slot, pump) = (source_slot.clone(), Arc::clone(&pump));
        rdp_direct::connect(
            b,
            to_a,
            Duration::from_secs(10),
            Box::new(to_source_pump.clone()),
            Box::new(move |_| {
                slot.set(None);
                pump.rewind();
            }),
        )
        .unwrap()
    };
    sink_slot.set(Some(accepted.join().unwrap().unwrap()));
    source_slot.set(Some(connected.clone()));

    // Read it all; cut the link half way.
    let reader = {
        let inflow = Arc::clone(&inflow);
        std::thread::spawn(move || {
            let mut out = Vec::with_capacity(LEN);
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                let n = inflow
                    .read(0, out.len() as u64, &mut buf, Duration::from_secs(20))
                    .unwrap();
                if n == 0 {
                    return out;
                }
                out.extend_from_slice(&buf[..n]);
                // A slow reader: acks hold the source back, so it's mid-stream at the cut.
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while inflow.throughput().bytes < (LEN / 2) as u64 {
        assert!(Instant::now() < deadline, "the first half never arrived");
        std::thread::sleep(Duration::from_millis(1));
    }
    connected.close();
    let received = reader.join().unwrap();
    assert!(received == *data, "the file arrived intact");
    assert!(
        fallback_data.load(std::sync::atomic::Ordering::SeqCst) > 0,
        "the rest came over the channel"
    );
    assert!(!source_slot.get().is_some_and(|l| l.is_up()), "the link is gone");
}
