extern crate clap;
extern crate byteorder;
extern crate app_dirs;
extern crate serde_json;
extern crate base64;
extern crate tiny_keccak;
extern crate serde;
#[macro_use]
extern crate serde_derive;

use std::fs;
use std::fs::DirEntry;
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

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with(".")
}

fn visit_dirs(dir: &Path, cb: &mut FnMut(&DirEntry)) -> std::io::Result<()> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if is_hidden(&path) {
                continue;
            }

            if path.is_dir() {
                visit_dirs(&path, cb)?;
            } else {
                cb(&entry);
            }
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    println!("{:?}", path);

    let mut file = File::open(&path)?;

    let mut buffer = [0; 4096];
    let mut hasher = tiny_keccak::Keccak::new_sha3_256();
    while file.read(&mut buffer)? > 0 {
        hasher.update(&buffer);
    }

    let mut res = [0; 32];
    hasher.finalize(&mut res);

    Ok(base64::encode(&res))
}

#[derive(Debug, Serialize, Deserialize)]
struct FolderEntry {
    path: String,
    hash: String,
}

impl FolderEntry {
    fn from_path(path: &Path) -> std::io::Result<Self> {
        Ok(FolderEntry {
               path: path.to_str().unwrap().to_owned(),
               hash: hash_file(path)?,
           })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct FolderSignature {
    entries: Vec<FolderEntry>,
}

impl FolderSignature {
    fn new() -> Self {
        FolderSignature { entries: vec![] }
    }

    fn add_entry(&mut self, path: &Path) -> std::io::Result<()> {
        self.entries.push(FolderEntry::from_path(path)?);
        Ok(())
    }
}

fn create_sig_files(cwd: &Path) -> FolderSignature {
    //make a sig file for each file in the current directory
    //TODO keep the sigs, only redo them if the timestamp of the file is newer
    let mut tmp_dir = std::env::temp_dir();
    tmp_dir.push("remote_python");
    fs::create_dir_all(&tmp_dir).unwrap();

    let mut sig = FolderSignature::new();

    visit_dirs(&cwd,
               &mut |entry| { sig.add_entry(&entry.path()).unwrap(); })
            .unwrap();

    sig
}

fn receive_request(socket: &mut TcpStream) -> std::io::Result<()> {

    //create or open the cwd
    let cwd = app_dir(AppDataType::UserConfig, &APP_INFO, "").unwrap();

    //create all sig_files in a given folder
    let sig = create_sig_files(&cwd);

    //send the request over to the client so it can send new data
    serde_json::to_writer(socket.try_clone()?, &sig)?;

    //pack and send over all of them

    //receive deltas

    //apply all deltas

    //run python


    //read how many bytes it will be
    let len = socket.read_u64::<LittleEndian>()?;

    //read the bytes
    let mut code = String::with_capacity(len as usize);
    socket.take(len).read_to_string(&mut code)?;

    //run python
    let child = Command::new(find_python_version())
        .args(&["-u", "-c", &code])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&cwd)
        .spawn()?;

    socket.write(b"Python execution launched\n")?;

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

    Ok(())
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

        //create or open the cwd
        let cwd = app_dir(AppDataType::UserConfig, &APP_INFO, "").unwrap();

        //create all sig_files in a given folder
        let sig = create_sig_files(&cwd);

        let sig_json = serde_json::to_string(&sig).unwrap();

        println!("{}", sig_json);

        println!("Opening server at {}", address);

        let listener = TcpListener::bind(address).unwrap();

        for stream in listener.incoming() {
            println!("Handling request");
            match stream {
                Ok(mut stream) => {
                    if let Err(e) = receive_request(&mut stream) {
                        println!("Something went wrong: {:?}", e);
                    }
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