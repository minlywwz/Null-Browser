use std::io::{self, Read, Write};

use display_list::{TextMetrics, TextRange, TextRun, TextStyle};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use web_engine::{LaidOutPage, Viewport};

const MAX_FRAME: usize = 40 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub enum Request {
    Open(String),
    Measured(Result<TextMetrics, String>),
    Layout {
        viewport: Viewport,
        assets: Option<Assets>,
    },
    Click {
        node: u32,
        viewport: Viewport,
    },
    Probe {
        private_file: String,
        loopback_port: u16,
    },
    FormInput {
        node: u32,
        value: String,
        viewport: Viewport,
    },
    FormToggle {
        node: u32,
        viewport: Viewport,
    },
    FormRadio {
        node: u32,
        viewport: Viewport,
    },
    SubmitForm {
        node: u32,
    },
}

#[derive(Serialize, Deserialize)]
pub enum Reply {
    Resources(Resources),
    Measure(Text),
    Page(Snapshot),
    Probe(Probe),
    Submitted {
        action: String,
        query: String,
    },
    Error(String),
}

#[derive(Serialize, Deserialize)]
pub struct Resources {
    pub script_shell: bool,
    pub stylesheets: Vec<String>,
    pub images: Vec<String>,
    pub scripts: Vec<String>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Assets {
    pub stylesheets: Vec<(String, String)>,
    pub images: Vec<(String, Vec<u8>)>,
    pub scripts: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Probe {
    pub file_blocked: bool,
    pub network_blocked: bool,
    pub child_blocked: bool,
}

#[derive(Serialize, Deserialize)]
pub struct Snapshot {
    pub title: String,
    pub page: LaidOutPage,
    pub images: Option<Vec<Pixels>>,
}

#[derive(Serialize, Deserialize)]
pub struct Text {
    pub value: String,
    pub style: TextStyle,
    pub width: f32,
    pub links: Vec<TextRange>,
    pub runs: Vec<TextRun>,
    pub wrap: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Pixels {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

pub fn transmit<T: Serialize>(output: &mut impl Write, message: &T) -> io::Result<()> {
    let data = bincode::serde::encode_to_vec(
        message,
        bincode::config::standard().with_limit::<MAX_FRAME>(),
    )
    .map_err(io::Error::other)?;
    if data.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "IPC frame exceeds its limit.",
        ));
    }
    output.write_all(&(data.len() as u32).to_le_bytes())?;
    output.write_all(&data)?;
    output.flush()
}

pub fn receive<T: DeserializeOwned>(input: &mut impl Read) -> io::Result<Option<T>> {
    let mut header = [0; 4];
    if input.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    input.read_exact(&mut header[1..])?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid IPC frame length.",
        ));
    }
    let mut data = vec![0; length];
    input.read_exact(&mut data)?;
    let (message, used) = bincode::serde::decode_from_slice(
        &data,
        bincode::config::standard().with_limit::<MAX_FRAME>(),
    )
    .map_err(io::Error::other)?;
    if used != length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Extra bytes in IPC frame.",
        ));
    }
    Ok(Some(message))
}
