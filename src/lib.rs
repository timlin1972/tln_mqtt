use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anstream::println;
use owo_colors::OwoColorize as _;
use regex::Regex;
use rumqttc::{Client, Event, LastWill, MqttOptions, Outgoing, Packet, QoS};
use serde::{Deserialize, Serialize};
use tracing::{error, span, trace, Level};

use common::{cfg, plugin, utils};

mod event;

const MODULE: &str = "mqtt";
const MODULE_RX: &str = "mqtt::rx";
const BROKER: &str = "broker.emqx.io";
const RESTART_DELAY: u64 = 20;
const SHOW: &str = r#"
action plugin mqtt report '{"topic": "tln/pi5/send", "payload": "exit"}'
    Ask a remote device to exit.

action plugin mqtt report '{"topic": "tln/HomeUbuntu/echo", "payload": "Hello"}'
    Echo 'Hello' to device 'HomeUbuntu'

action plugin mqtt remote command
    Interative command
"#;

#[derive(Serialize, Deserialize, Debug)]
struct Report {
    topic: String,
    payload: String,
}

pub struct Plugin {
    client: Arc<Mutex<rumqttc::Client>>,
    // connection: Arc<Mutex<rumqttc::Connection>>,
    print_event: Arc<Mutex<bool>>,
}

impl Plugin {
    pub fn new(tx: &crossbeam_channel::Sender<String>) -> Plugin {
        println!("[{}] Loading...", MODULE.blue());

        let _ = tracing_subscriber::fmt::try_init();
        let span = span!(Level::INFO, MODULE);
        let _enter = span.enter();

        // Set MQTT connection options and last will message
        let mut mqttoptions = MqttOptions::new(cfg::get_name(), BROKER, 1883);
        let will = LastWill::new(
            format!("tln/{}/onboard", cfg::get_name()),
            "0",
            QoS::AtMostOnce,
            true,
        );
        mqttoptions
            .set_keep_alive(Duration::from_secs(5))
            .set_last_will(will);

        let (client, mut connection) = Client::new(mqttoptions, 10);

        // clear center_default
        publish(&client, &format!("tln/{}/onboard", cfg::DEF_NAME), true, "");

        // subscribe
        subscribe(&client, "tln/#");

        // publish onboard
        publish(
            &client,
            &format!("tln/{}/onboard", cfg::get_name()),
            true,
            "1",
        );

        let print_event = Arc::new(Mutex::new(true));
        let print_event_clone = Arc::clone(&print_event);

        let client = Arc::new(Mutex::new(client));
        let tx_clone = tx.clone();
        thread::spawn(move || {
            let span = span!(Level::INFO, MODULE_RX);
            let _enter = span.enter();

            println!("[{}] Start to receive mqtt message (but ignore the outgoing ping req and incoming ping resp).", MODULE.blue());
            for notification in connection.iter() {
                match notification {
                    Ok(notif) => {
                        if Event::Outgoing(Outgoing::Disconnect) == notif {
                            println!("[{}] Notification = {notif:?}, leave", MODULE.blue(),);
                            return;
                        }
                        let print_event = print_event_clone.lock().unwrap();
                        event::print_event(&notif, &tx_clone, *print_event);
                        process_event(&notif, &tx_clone);
                    }
                    Err(e) => {
                        let log = format!(
                            "[{}] Mqtt rx err: Notification = {:?}",
                            MODULE.blue(),
                            e.red()
                        );
                        println!("{log}");
                        tx_clone
                            .send(format!("send plugin logs add '{}'", utils::encrypt(&log)))
                            .unwrap();

                        let log = format!(
                            "[{}] Sleep {} seconds to restart...",
                            MODULE.blue(),
                            RESTART_DELAY
                        );
                        println!("{log}");
                        tx_clone
                            .send(format!("send plugin logs add '{}'", utils::encrypt(&log)))
                            .unwrap();

                        std::thread::sleep(Duration::from_secs(RESTART_DELAY));
                        tx_clone.send("restart plugin mqtt".to_owned()).unwrap();
                        return;
                    }
                }
            }
        });

        Plugin {
            client,
            // connection
            print_event,
        }
    }
}

impl plugin::Plugin for Plugin {
    fn name(&self) -> &str {
        MODULE
    }

    fn show(&mut self) -> String {
        println!("[{}]", MODULE.blue());

        let mut show = String::new();
        show += SHOW;

        println!("{show}");

        show
    }

    fn status(&mut self) -> String {
        println!("[{}]", MODULE.blue());

        let mut status = String::new();

        status += &format!("Broker: {}\n", BROKER);
        status += &format!("Id: {}\n", cfg::get_name());

        println!("{status}");

        status
    }

    fn send(&mut self, action: &str, data: &str) -> String {
        if action == "report" {
            let client = self.client.lock().unwrap();
            let report: Report = serde_json::from_str(data).unwrap();
            let payload = &report.payload;
            publish(&client, &report.topic, false, payload);
        }

        "send".to_owned()
    }

    fn action(&mut self, action: &str, data: &str, _data2: &str) -> String {
        if action == "report" {
            let client = self.client.lock().unwrap();
            if data == "myself" {
                publish(
                    &client,
                    &format!("tln/{}/onboard", cfg::get_name()),
                    true,
                    "1",
                );
            } else {
                let report: Report = serde_json::from_str(data).unwrap();
                let payload = &report.payload;
                let payload = utils::encrypt(payload);

                publish(&client, &report.topic, false, &payload);
            }
        }

        if action == "remote" && data == "command" {
            let client = self.client.lock().unwrap();

            // get remote
            print!("Please enter remote device: ");
            io::stdout().flush().unwrap();

            let mut remote = String::new();
            io::stdin().read_line(&mut remote).unwrap();
            let remote = remote.trim();

            let topic = format!("tln/{remote}/send");

            // set remote dest
            let payload =
                utils::encrypt(&format!("action plugin command dest '{}'", cfg::get_name()));
            publish(&client, &topic, false, &payload);

            // remote shell start
            let payload = utils::encrypt("action plugin command shell start");
            publish(&client, &topic, false, &payload);

            // turn off the print event
            let last_print_event = {
                let mut print_event = self.print_event.lock().unwrap();
                let last_print_event = *print_event;
                *print_event = false;
                last_print_event
            };

            loop {
                print!("{remote} > ");
                io::stdout().flush().unwrap();

                let mut input = String::new();
                io::stdin().read_line(&mut input).unwrap();
                let input = input.trim();

                if input.is_empty() {
                    continue;
                }

                if input == "exit" {
                    println!("Exiting...");
                    break;
                }

                let payload = utils::encrypt(&format!("action plugin command cmd '{input}'"));

                publish(&client, &topic, false, &payload);
            }

            // turn on the print event
            {
                let mut print_event = self.print_event.lock().unwrap();
                *print_event = last_print_event;
            };

            // remote shell stop
            let payload = utils::encrypt("action plugin command shell stop");
            publish(&client, &topic, false, &payload);
        }

        "action".to_owned()
    }

    fn unload(&mut self) -> String {
        println!("[{}] Unload", MODULE.blue());

        let client = self.client.lock().unwrap();

        // send onboard = false
        publish(
            &client,
            &format!("tln/{}/onboard", cfg::get_name()),
            true,
            "0",
        );

        let _ = client.disconnect();

        "unload".to_owned()
    }
}

#[no_mangle]
pub extern "C" fn create_plugin(
    tx: &crossbeam_channel::Sender<String>,
) -> *mut plugin::PluginWrapper {
    let plugin = Box::new(Plugin::new(tx));
    Box::into_raw(Box::new(plugin::PluginWrapper::new(plugin)))
}

/// # Safety
#[no_mangle]
pub unsafe extern "C" fn unload_plugin(wrapper: *mut plugin::PluginWrapper) {
    if !wrapper.is_null() {
        unsafe {
            let _ = Box::from_raw(wrapper);
        }
    }
}

fn subscribe(client: &rumqttc::Client, topic: &str) {
    trace!("Subscribe: '{topic}'");
    client.subscribe(topic, QoS::AtMostOnce).unwrap();
}

fn publish(client: &rumqttc::Client, topic: &str, retain: bool, payload: &str) {
    trace!("Publish: '{topic}::{payload}'");
    if client
        .publish(topic, QoS::AtLeastOnce, retain, payload)
        .is_err()
    {
        error!("Failed to publish: '{topic}::{payload}'");
    }
}

fn process_event(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    process_event_onboard(notif, tx);
    process_event_uptime(notif, tx);
    process_event_hostname(notif, tx);
    process_event_os(notif, tx);
    event::process_event_send(notif, tx);
    process_event_temperature(notif, tx);
    process_event_sw_uptime(notif, tx);
    process_event_echo(notif, tx);
}

#[derive(Serialize, Deserialize, Default)]
struct CmdUpdate {
    name: String,
    onboard: Option<bool>,
    uptime: Option<u64>,
    hostname: Option<String>,
    os: Option<String>,
    temperature: Option<f32>,
    sw_uptime: Option<u64>,
}

fn process_event_onboard(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        let re = Regex::new(r"^tln/([^/]+)/onboard$").unwrap();
        if let Some(captures) = re.captures(topic) {
            if let Some(name) = captures.get(1) {
                let payload = std::str::from_utf8(&publish.payload).unwrap();
                let onboard = match payload.parse::<u64>() {
                    Ok(t) => t,
                    Err(_) => return,
                };
                if onboard != 0 && onboard != 1 {
                    return;
                }
                let cmd_update = CmdUpdate {
                    name: name.as_str().to_owned(),
                    onboard: Some(onboard == 1),
                    ..Default::default()
                };
                let json_string = serde_json::to_string(&cmd_update).unwrap();
                tx.send(format!("action plugin devinfo update '{json_string}'"))
                    .unwrap();
            }
        }
    }
}

fn process_event_uptime(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        let re = Regex::new(r"^tln/([^/]+)/uptime$").unwrap();
        if let Some(captures) = re.captures(topic) {
            if let Some(name) = captures.get(1) {
                let payload = std::str::from_utf8(&publish.payload).unwrap();
                let uptime = match payload.parse::<u64>() {
                    Ok(t) => t,
                    Err(_) => return,
                };

                let cmd_update = CmdUpdate {
                    name: name.as_str().to_owned(),
                    uptime: Some(uptime),
                    ..Default::default()
                };
                let json_string = serde_json::to_string(&cmd_update).unwrap();
                tx.send(format!("action plugin devinfo update '{json_string}'"))
                    .unwrap();
            }
        }
    }
}

fn process_event_hostname(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        let re = Regex::new(r"^tln/([^/]+)/hostname$").unwrap();
        if let Some(captures) = re.captures(topic) {
            if let Some(name) = captures.get(1) {
                let hostname = std::str::from_utf8(&publish.payload).unwrap();
                let cmd_update = CmdUpdate {
                    name: name.as_str().to_owned(),
                    hostname: Some(hostname.to_owned()),
                    ..Default::default()
                };
                let json_string = serde_json::to_string(&cmd_update).unwrap();
                tx.send(format!("action plugin devinfo update '{json_string}'"))
                    .unwrap();
            }
        }
    }
}

fn process_event_os(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        let re = Regex::new(r"^tln/([^/]+)/os$").unwrap();
        if let Some(captures) = re.captures(topic) {
            if let Some(name) = captures.get(1) {
                let os = std::str::from_utf8(&publish.payload).unwrap();
                let cmd_update = CmdUpdate {
                    name: name.as_str().to_owned(),
                    os: Some(os.to_owned()),
                    ..Default::default()
                };
                let json_string = serde_json::to_string(&cmd_update).unwrap();
                tx.send(format!("action plugin devinfo update '{json_string}'"))
                    .unwrap();
            }
        }
    }
}

fn process_event_temperature(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        let re = Regex::new(r"^tln/([^/]+)/temperature$").unwrap();
        if let Some(captures) = re.captures(topic) {
            if let Some(name) = captures.get(1) {
                let payload = std::str::from_utf8(&publish.payload).unwrap();
                let temperature = match payload.parse::<f32>() {
                    Ok(t) => t,
                    Err(_) => return,
                };

                let cmd_update = CmdUpdate {
                    name: name.as_str().to_owned(),
                    temperature: Some(temperature),
                    ..Default::default()
                };
                let json_string = serde_json::to_string(&cmd_update).unwrap();
                tx.send(format!("action plugin devinfo update '{json_string}'"))
                    .unwrap();
            }
        }
    }
}

fn process_event_sw_uptime(notif: &Event, tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        let re = Regex::new(r"^tln/([^/]+)/sw_uptime$").unwrap();
        if let Some(captures) = re.captures(topic) {
            if let Some(name) = captures.get(1) {
                let payload = std::str::from_utf8(&publish.payload).unwrap();
                let sw_uptime = match payload.parse::<u64>() {
                    Ok(t) => t,
                    Err(_) => return,
                };

                let cmd_update = CmdUpdate {
                    name: name.as_str().to_owned(),
                    sw_uptime: Some(sw_uptime),
                    ..Default::default()
                };
                let json_string = serde_json::to_string(&cmd_update).unwrap();
                tx.send(format!("action plugin devinfo update '{json_string}'"))
                    .unwrap();
            }
        }
    }
}

fn process_event_echo(notif: &Event, _tx: &crossbeam_channel::Sender<String>) {
    if let Event::Incoming(Packet::Publish(publish)) = notif {
        let topic = &publish.topic;

        if topic == &format!("tln/{}/echo", cfg::get_name()) {
            let payload = std::str::from_utf8(&publish.payload).unwrap();
            println!("{payload}");
        }
    }
}
