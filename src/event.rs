use anstream::println;
use owo_colors::OwoColorize as _;
use rumqttc::{Event, Outgoing, Packet, Publish};
use tracing::info;

use common::{cfg, utils};

pub fn print_event(notif: &Event, tx: &crossbeam_channel::Sender<String>, print_event: bool) {
    fn print_event_routine(
        tx: &crossbeam_channel::Sender<String>,
        publish: &Publish,
        log_to_log: bool,
    ) {
        let payload = std::str::from_utf8(&publish.payload).unwrap();
        let payload = if publish.topic.contains("send") {
            &utils::decrypt(payload)
        } else {
            payload
        };
        let log = format!(
            "{}: {}::{payload}",
            "Incoming publish".green(),
            publish.topic
        );
        info!(log);

        if log_to_log {
            let log = utils::encrypt(&log);
            tx.send(format!("send plugin logs add '{log}'")).unwrap();
        }
    }

    match notif {
        Event::Incoming(Packet::PingResp)
        | Event::Outgoing(Outgoing::PingReq)
        | Event::Incoming(Packet::ConnAck(_))
        | Event::Incoming(Packet::SubAck(_))
        | Event::Outgoing(Outgoing::Subscribe(_))
        | Event::Outgoing(Outgoing::Publish(_))
        | Event::Incoming(Packet::PubAck(_)) => (),
        Event::Incoming(Packet::Publish(publish)) => {
            if publish.topic.contains("send") && print_event {
                print_event_routine(tx, publish, true);
            }
        }
        _ => {
            println!("Err: Notification = {:?}", notif.red());
        }
    }
}

pub fn process_event_send(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        if topic == &format!("tln/{}/send", cfg::get_name()) {
            let payload = utils::decrypt(std::str::from_utf8(&publish.payload).unwrap());
            tx.send(payload.to_owned()).unwrap();
        }
    }
}
