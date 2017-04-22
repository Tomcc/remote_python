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
use std::thread;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::io::Result;
use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use app_dirs::*;

const APP_INFO: AppInfo = AppInfo {
    name: "remote_python",
    author: "tommaso",
};

fn hash_file(path: &Path) -> std::io::Result<String> {
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

#[derive(Debug, Serialize, Deserialize, Eq, PartialEq)]
struct FolderEntry {
    path: String,
    hash: String,
}

impl FolderEntry {
    fn from_path(rel_path: &Path, root: &Path) -> std::io::Result<Self> {
        Ok(FolderEntry {
               path: rel_path.to_str().unwrap().to_owned(),
               hash: hash_file(&root.join(rel_path))?,
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

    fn add_entry(&mut self, path: &Path, root: &Path) -> std::io::Result<()> {
        self.entries.push(FolderEntry::from_path(path, root)?);
        Ok(())
    }

    fn contains(&self, entry: &FolderEntry) -> bool {
        for e in &self.entries {
            if e == entry {
                return true;
            }
        }
        false
    }

    fn new_and_changed(&self, other: &FolderSignature) -> Vec<PathBuf> {
        //for each entry, check if it's in the other
        let mut paths = vec![];
        for entry in &self.entries {
            if !other.contains(&entry) {
                //we'll need to send it
                paths.push(PathBuf::from(&entry.path));
            }
        }

        paths
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct FileHeader {
    length: usize,
    rel_path: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ClientCommand {
    python_file_path: String,
    num_files: usize,
}

fn send_json<T: serde::Serialize>(socket: &mut TcpStream, val: &T) -> std::io::Result<()> {
    //serialize to string
    let buf = serde_json::to_string(val)?;

    //send length and body
    socket
        .write_u64::<LittleEndian>(buf.as_bytes().len() as u64)?;
    socket.write_all(buf.as_bytes())?;

    Ok(())
}

fn receive_json<T: serde::de::DeserializeOwned>(socket: &mut TcpStream) -> std::io::Result<T> {
    //read the length
    let len = socket.read_u64::<LittleEndian>()?;
    let mut buf = String::with_capacity(len as usize);

    socket.take(len).read_to_string(&mut buf)?;

    Ok(serde_json::from_str::<T>(&buf).unwrap())
}

fn send_file(socket: &mut TcpStream, rel_path: &Path, root: &Path) -> std::io::Result<()> {
    let abs_path = root.join(rel_path);

    let mut file = File::open(abs_path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;

    let header = FileHeader {
        length: buf.len(),
        rel_path: rel_path.to_str().unwrap().to_owned(),
    };

    //send the file metadata
    send_json(socket, &header)?;

    socket.write_all(&buf)?;

    Ok(())
}

fn client_function(socket: &mut TcpStream, python_path: &Path) -> std::io::Result<()> {
    //receive the status of files on the server
    let server_sig: FolderSignature = receive_json(socket)?;

    //make our own folder signature
    let cwd = std::env::current_dir()?;
    let local_sig = create_sig_files(&cwd);

    //make a diff, find new files and changed files
    let diff = local_sig.new_and_changed(&server_sig);

    //tell the server how many files to expect
    let command = ClientCommand {
        python_file_path: python_path.to_str().unwrap().to_owned(),
        num_files: diff.len(),
    };

    send_json(socket, &command)?;

    //send all files
    for rel_path in diff {
        send_file(socket, &rel_path, &cwd)?;
    }

    //and now wait for the results of the command
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

    Ok(())
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

fn create_sig_files(cwd: &Path) -> FolderSignature {
    //make a sig file for each file in the current directory
    //TODO keep the sigs, only redo them if the timestamp of the file is newer
    let mut tmp_dir = std::env::temp_dir();
    tmp_dir.push("remote_python");
    fs::create_dir_all(&tmp_dir).unwrap();

    let mut sig = FolderSignature::new();

    visit_dirs(&cwd,
               &mut |entry| {
                        let abs_path = entry.path();
                        let rel_path = abs_path.strip_prefix(&cwd).unwrap();
                        sig.add_entry(&rel_path, &cwd).unwrap();
                    })
            .unwrap();

    sig
}

fn download_files(socket: &mut TcpStream, root: &Path, num_files: usize) -> std::io::Result<()> {
    //attempt to download that many files
    for _ in 0..num_files {
        let header: FileHeader = receive_json(socket)?;
        let abs_path = root.join(&header.rel_path);

        fs::create_dir_all(abs_path.parent().unwrap()).unwrap();

        let mut file = File::create(&abs_path)?;
        let mut buffer = Vec::new();
        socket
            .take(header.length as u64)
            .read_to_end(&mut buffer)?;
        file.write_all(&buffer)?;
    }
    Ok(())
}

fn server_function(socket: &mut TcpStream) -> std::io::Result<()> {

    //create or open the cwd
    let cwd = app_dir(AppDataType::UserConfig, &APP_INFO, "").unwrap();
    println!("Executing into {:?}", cwd);

    //create all sig_files in a given folder
    let sig = create_sig_files(&cwd);
    println!("Available files {:?}", sig);

    //send the request over to the client so it can send new data
    send_json(socket, &sig)?;

    //receive the client data
    let client_command: ClientCommand = receive_json(socket)?;

    println!("Client command: {:?}", client_command);

    //now receive the files that the client wants to write one by one
    if client_command.num_files > 0 {
        socket.write(b"Downloading new files... ")?;
        download_files(socket, &cwd, client_command.num_files)?;
        socket.write(b"Done\n")?;
    }

    //run python on the file indicated by the client
    let child = Command::new(find_python_version())
        .args(&["-u", &client_command.python_file_path])
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
        let listener = TcpListener::bind(&address).unwrap();
        println!("Opened server at {}", address);

        for stream in listener.incoming() {
            println!("Handling request");
            match stream {
                Ok(mut stream) => {
                    if let Err(e) = server_function(&mut stream) {
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

                if let Err(e) = client_function(&mut stream,
                                                Path::new(matches.value_of("file_path").unwrap())) {
                    println!("Something went wrong {}", e);
                }
            }
            Err(_) => println!("Connection failed"),
        }
    }
}