extern crate clap;
extern crate byteorder;
extern crate app_dirs;

use std::process::Command;
use std::process::Stdio;
use std::io::prelude::*;
use std::net::TcpStream;
use std::net::TcpListener;
use std::path::Path;
use std::fs::File;
use clap::{Arg, App};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::thread;
use std::sync::mpsc::channel;
use std::io::Result;
use app_dirs::*;

const APP_INFO: AppInfo = AppInfo {
    name: "remote_python",
    author: "tommaso",
};

fn send_request(socket: &mut Write, pathstr: &str) {
    let path = Path::new(pathstr);

    if let Ok(mut file) = File::open(path) {
        let len = file.metadata().unwrap().len();

        socket.write_u64::<LittleEndian>(len).unwrap();
        std::io::copy(&mut file, socket).unwrap();
    } else {
        println!("Error: Cannot open path {}", pathstr);
    }
}

fn receive_response(socket: &mut Read) {
    let reader = std::io::BufReader::new(socket);
    for line_result in reader.lines() {
        match line_result {
            Ok(line) => {
                println!("{}", line);
            }
            Err(e) => {
                println!("Child error: {}", e);
                break;
            }
        };
    }
}

fn find_python_version() -> &'static str {
    if let Ok(_) = Command::new("python3").status() {
        return "python3";
    }
    return "python";
}

fn write_server_response(socket: &mut TcpStream, line: &str) -> Result<usize> {
    socket.write(line.as_bytes())?;
    socket.write(b"\n")?;
    socket.flush()?;

    Ok(0)
}

fn handle_output<T: Read + Send + 'static>(stream: T, tx: std::sync::mpsc::Sender<String>) {
    let child_buf = std::io::BufReader::new(stream);
    thread::spawn(move || for line_result in child_buf.lines() {
                      match line_result {
                          Ok(line) => {
                              if let Err(_) = tx.send(line) {
                                  return;
                              }
                          }
                          Err(e) => {
            println!("Pipe error: {}", e);
            break;
        }
                      };
                  });
}

fn receive_request(socket: &mut TcpStream) {
    //read how many bytes it will be
    let len = socket.read_u64::<LittleEndian>().unwrap();

    //read the bytes
    let mut code = String::with_capacity(len as usize);
    socket.take(len).read_to_string(&mut code).unwrap();

    //create the cwd
    let cwd = app_dir(AppDataType::UserConfig, &APP_INFO, "").unwrap();

    //run python
    let child = Command::new(find_python_version())
        .args(&["-u", "-c", &code])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&cwd)
        .spawn()
        .unwrap();

    socket.write(b"Python execution launched\n").unwrap();

    let (tx, rx) = channel();

    handle_output(child.stdout.unwrap(), tx.clone());
    handle_output(child.stderr.unwrap(), tx);

    //receive all the interleaved channels until both have hung up
    loop {
        match rx.recv() {
            Ok(line) => {
                if let Err(_) = write_server_response(socket, &line) {
                    println!("Connection dropped, aborting");
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

fn main() {
    let matches = App::new("Remote Python")
        .version("0.1")
        .about("Because reasons")
        .arg(Arg::with_name("port")
            .short("p")
            .long("port")
            .value_name("PORT")
            .help("The port to use to connect to the server or to host to it. Default value is \
                   55455")
            .default_value("55455")
            .takes_value(true))
        .arg(Arg::with_name("server")
            .long("server")
            .help("If this is specified, this will be a server"))
        .arg(Arg::with_name("address")
            .value_name("ADDRESS")
            .help("Address to connect to")
            .required(true))
        .arg(Arg::with_name("file_path").help("Python file path"))
        .get_matches();

    let port = matches.value_of("port").unwrap();

    let address = matches.value_of("address").unwrap();
    let address = format!("{}:{}", address, port);

    if matches.is_present("server") {
        println!("Opening server at {}", address);

        let listener = TcpListener::bind(address).unwrap();

        for stream in listener.incoming() {
            println!("Handling request");
            match stream {
                Ok(mut stream) => {
                    receive_request(&mut stream);
                }
                Err(_) => { /* connection failed */ }
            }
            println!("Done");
        }
    } else {

        print!("Connecting to {}... ", address);
        std::io::stdout().flush().unwrap();

        match TcpStream::connect(address) {
            Ok(mut stream) => {
                println!("Connected");

                send_request(&mut stream, matches.value_of("file_path").unwrap());
                receive_response(&mut stream);
            }
            Err(_) => println!("Connection failed"),
        }
    }
}