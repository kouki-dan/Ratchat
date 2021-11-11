use futures::{Stream, StreamExt};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;
use warp::{sse::Event, Filter};

#[tokio::main]
async fn main() {
    pretty_env_logger::init();

    // Keep track of all connected users, key is usize, value
    // is an event stream sender.
    let users = Arc::new(Mutex::new(HashMap::new()));
    // Turn our "state" into a new Filter...
    let users = warp::any().map(move || users.clone());

    let messages = Arc::new(Mutex::new(HashMap::new()));
    let messages = warp::any().map(move || messages.clone());

    // POST /room/:name/chat -> send message
    let chat_send = warp::path!("room" / String / "chat" / usize)
        .and(warp::post())
        .and(warp::body::content_length_limit(500))
        .and(
            warp::body::bytes().and_then(|body: bytes::Bytes| async move {
                std::str::from_utf8(&body)
                    .map(String::from)
                    .map_err(|_e| warp::reject::custom(NotUtf8))
            }),
        )
        .and(users.clone())
        .and(messages.clone())
        .map(|room_name, my_id, msg, users, messages| {
            user_message(&room_name, my_id, &msg, &users, &messages);
            warp::reply()
        });

    // GET /room/:name/chat -> messages stream
    let chat_recv = warp::path!("room" / String / "chat")
        .and(warp::get())
        .and(users)
        .and(messages)
        .map(|room_name, users, messages| {
            // reply using server-sent events
            let stream = user_connected(&room_name, users, messages);
            warp::sse::reply(warp::sse::keep_alive().stream(stream))
        });

    // GET /room/:name -> chat html
    let room = warp::path!("room" / String).map(|_| {
        warp::http::Response::builder()
            .header("content-type", "text/html; charset=utf-8")
            .body(CHAT_HTML)
    });

    // GET / -> index html
    let index = warp::path::end().map(|| {
        warp::http::Response::builder()
            .header("content-type", "text/html; charset=utf-8")
            .body(INDEX_HTML)
    });

    let routes = index.or(room).or(chat_recv).or(chat_send);

    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}

/// Our global unique user id counter.
static NEXT_USER_ID: AtomicUsize = AtomicUsize::new(1);

/// Message variants.
#[derive(Debug)]
enum Message {
    UserId(usize),
    Reply(String),
}

#[derive(Debug)]
struct NotUtf8;
impl warp::reject::Reject for NotUtf8 {}

/// Our state of currently connected users.
type RoomKey = String;
type UserId = usize;
type Users = Arc<Mutex<HashMap<RoomKey, HashMap<UserId, mpsc::UnboundedSender<Message>>>>>;
type Messages = Arc<Mutex<HashMap<RoomKey, Vec<String>>>>;

fn user_connected(
    room_name: &String,
    users: Users,
    messages: Messages,
) -> impl Stream<Item = Result<Event, warp::Error>> + Send + 'static {
    // Use a counter to assign a new unique ID for this user.
    let my_id = NEXT_USER_ID.fetch_add(1, Ordering::Relaxed);

    // Use an unbounded channel to handle buffering and flushing of messages
    // to the event source...
    let (tx, rx) = mpsc::unbounded_channel();
    let rx = UnboundedReceiverStream::new(rx);

    tx.send(Message::UserId(my_id))
        // rx is right above, so this cannot fail
        .unwrap();

    let mut rooms = messages.lock().unwrap();
    let room = rooms.get(room_name);
    match room {
        Some(room) => {
            for message in room {
                tx.send(Message::Reply(message.to_string())).unwrap();
            }
        }
        None => {
            let new_room = Vec::new();
            rooms.insert(room_name.clone(), new_room);
        }
    }

    // Save the sender in our list of connected users.
    let mut rooms = users.lock().unwrap();
    let room = rooms.get_mut(room_name);
    match room {
        Some(room) => {
            room.insert(my_id, tx);
        }
        None => {
            let mut new_room = HashMap::new();
            new_room.insert(my_id, tx);
            rooms.insert(room_name.clone(), new_room);
        }
    }

    // Convert messages into Server-Sent Events and return resulting stream.
    rx.map(|msg| match msg {
        Message::UserId(my_id) => Ok(Event::default().event("user").data(my_id.to_string())),
        Message::Reply(reply) => Ok(Event::default().data(reply)),
    })
}

fn user_message(
    room_name: &String,
    my_id: usize,
    msg: &String,
    users: &Users,
    messages: &Messages,
) {
    let new_msg = format!("<User#{}>: {}", my_id, msg);

    // New message from this user, send it to everyone else (except same uid)...
    //
    // We use `retain` instead of a for loop so that we can reap any user that
    // appears to have disconnected.
    let mut rooms = users.lock().unwrap();
    let room = rooms.get_mut(room_name);
    match room {
        Some(room) => {
            room.retain(|uid, tx| {
                if my_id == *uid {
                    // don't send to same user, but do retain
                    true
                } else {
                    // If not `is_ok`, the SSE stream is gone, and so don't retain
                    tx.send(Message::Reply(new_msg.clone())).is_ok()
                }
            });
        }
        None => {
            println!("なんか変だけど何もしない")
        }
    }

    let mut rooms = messages.lock().unwrap();
    let room = rooms.get_mut(room_name);
    match room {
        Some(room) => room.push(new_msg.clone()),
        None => {
            println!("なんか変だけど何もしない")
        }
    }
}

static INDEX_HTML: &str = r#"
<!DOCTYPE html>
<html>
    <head>
        <title>Rust chat</title>
    </head>
    <body>
        <h1>Rust chat</h1>
        <script>
        function onSubmit() {
            setTimeout(() => {
                location.href="/room/"+document.getElementById("name").value;
            },10 );
            return false;
        }
        </script>
        <form onsubmit="onSubmit()">
            <input id="name" type="text" placeholder="room name" />
            <input type="submit">
        </form>
    </body>
</html>
"#;

static CHAT_HTML: &str = r#"
<!DOCTYPE html>
<html>
    <head>
        <title>Warp Chat</title>
    </head>
    <body>
        <h1>warp chat</h1>
        <div id="chat">
            <p><em>Connecting...</em></p>
        </div>
        <input type="text" id="text" />
        <button type="button" id="send">Send</button>
        <script type="text/javascript">
        var uri = 'http://' + location.host + location.pathname + '/chat';
        var sse = new EventSource(uri);
        function message(data) {
            var line = document.createElement('p');
            line.innerText = data;
            chat.appendChild(line);
        }
        sse.onopen = function() {
            chat.innerHTML = "<p><em>Connected!</em></p>";
        }
        var user_id;
        sse.addEventListener("user", function(msg) {
            user_id = msg.data;
        });
        sse.onmessage = function(msg) {
            message(msg.data);
        };
        send.onclick = function() {
            var msg = text.value;
            var xhr = new XMLHttpRequest();
            xhr.open("POST", uri + '/' + user_id, true);
            xhr.send(msg);
            text.value = '';
            message('<You>: ' + msg);
        };
        </script>
    </body>
</html>
"#;
