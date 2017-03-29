extern crate clap;
extern crate byteorder;

use std::process::Command;
use std::process::Stdio;
use std::io::prelude::*;
use std::net::TcpStream;
use std::net::TcpListener;
use std::path::Path;
use std::fs::File;
use clap::{Arg, App};
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};


fn send_request(socket: &mut Write, pathstr: &str) {
    let path = Path::new(pathstr);

    if let Ok(mut file) = File::open(path) {
        let len = file.metadata().unwrap().len();

        socket.write_u64::<LittleEndian>(len);
        std::io::copy(&mut file, socket);
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

fn receive_request(socket: &mut TcpStream) {
    //read how many bytes it will be
    let len = socket.read_u64::<LittleEndian>().unwrap();

    //read the bytes
    let mut code = String::with_capacity(len as usize);
    socket.take(len).read_to_string(&mut code);

    //run python
    let child = Command::new("python3")
        .args(&["-c", &code])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    socket.write(b"Python execution launched\n");

    let child_buf = std::io::BufReader::new(child.stdout.unwrap());

    for line_result in child_buf.lines() {
        match line_result {
            Ok(line) => {
                socket.write(line.as_bytes());
                socket.write(b"\n");
                socket.flush();
            }
            Err(e) => {
                println!("Child error: {}", e);
                break;
            }
        };
    }
}

fn main() {
    let matches = App::new("Remote Python")
        .version("0.1")
        .about("Because reasons")
        .arg(Arg::with_name("client")
            .short("c")
            .long("client")
            .value_name("SERVER_ADDRESS")
            .help("Connects to a server")
            .takes_value(true))
        .arg(Arg::with_name("file_path").help("Python file path"))
        .get_matches();

    if let Some(address) = matches.value_of("client") {
        print!("Connecting to {}... ", address);

        match TcpStream::connect(address) {
            Ok(mut stream) => {
                println!("Connected");

                send_request(&mut stream, matches.value_of("file_path").unwrap());
                receive_response(&mut stream);
            }
            Err(_) => println!("Cannot connect to {}", address),
        }
    } else {
        println!("Opening server!");

        let listener = TcpListener::bind("localhost:55455").unwrap();

        for stream in listener.incoming() {
            match stream {
                Ok(mut stream) => {
                    receive_request(&mut stream);
                }
                Err(e) => { /* connection failed */ }
            }
        }
    }
}