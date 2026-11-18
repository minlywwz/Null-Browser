use std::{
    ffi::c_void,
    fs::File,
    io::{self, Read},
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    ptr::{self, null, null_mut},
    sync::mpsc,
    thread,
    time::Duration,
};

use windows_sys::Win32::{
    Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation},
    Security::{
        FreeSid,
        Isolation::{CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName},
        PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES,
    },
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject, TerminateJobObject,
        },
        Pipes::CreatePipe,
        Threading::{
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CreateProcessW, DeleteProcThreadAttributeList,
            EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, InitializeProcThreadAttributeList,
            LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
            PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            TerminateProcess, UpdateProcThreadAttribute, WaitForSingleObject,
        },
        WindowsProgramming::PROCESS_CREATION_CHILD_PROCESS_RESTRICTED,
    },
};

use display_list::TextMeasurer;
use rendering_ffi::TextFactory;
use web_content_process::{Reply, Request, receive, transmit};

use super::validate::{Scene, prepare_snapshot};

type Exchange = (Operation, mpsc::SyncSender<io::Result<Outcome>>);

enum Operation {
    Call(Request),
    Layout { request: Request, generation: u32 },
}

enum Outcome {
    Reply(Reply),
    Page(Scene),
}

pub struct WebContentProcess {
    job: OwnedHandle,
    requests: mpsc::Sender<Exchange>,
}

impl WebContentProcess {
    pub fn start() -> Result<Self, String> {
        Self::launch()
            .map_err(|error| format!("Could not start the isolated Web Content Process. {error}"))
    }

    fn launch() -> io::Result<Self> {
        let profile = Profile::open()?;
        let capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: profile.0,
            Capabilities: null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let (child_read, parent_write) = pipe()?;
        let (parent_read, child_write) = pipe()?;
        let (stderr_read, child_error) = pipe()?;
        inheritable(&parent_write, false)?;
        inheritable(&parent_read, false)?;
        inheritable(&stderr_read, false)?;

        let handles = [raw(&child_read), raw(&child_write), raw(&child_error)];
        let child_policy = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
        let mut attributes = Attributes::new(3)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
        attributes.set(PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, &child_policy)?;

        let job = job()?;
        let executable = std::env::current_exe()?.with_file_name("web_content_process.exe");
        if !executable.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Place web_content_process.exe next to browser_app.exe.",
            ));
        }
        let mut command = vec![b'"' as u16];
        command.extend(executable.as_os_str().encode_wide());
        command.extend("\"\0".encode_utf16());
        let executable: Vec<_> = executable.as_os_str().encode_wide().chain([0]).collect();
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attributes.pointer();
        let mut info = PROCESS_INFORMATION::default();
        check(unsafe {
            CreateProcessW(
                executable.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW,
                null(),
                null(),
                &startup.StartupInfo,
                &mut info,
            )
        })?;
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        let thread_handle = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        if let Err(error) = check(unsafe { AssignProcessToJobObject(raw(&job), raw(&process)) }) {
            unsafe {
                TerminateProcess(raw(&process), 1);
            }
            return Err(error);
        }
        if unsafe { ResumeThread(raw(&thread_handle)) } == u32::MAX {
            return Err(io::Error::last_os_error());
        }
        drop((child_read, child_write, child_error));
        let (requests, queue) = mpsc::channel::<Exchange>();
        thread::Builder::new()
            .name("Web Content IPC".into())
            .spawn(move || {
                let mut output = File::from(parent_write);
                let mut input = File::from(parent_read);
                let mut errors = File::from(stderr_read);
                let mut images = Vec::new();
                let factory = TextFactory::new();
                while let Ok((operation, reply)) = queue.recv() {
                    let response = match operation {
                        Operation::Call(request) => transmit(&mut output, &request)
                            .and_then(|()| read_reply(&mut input))
                            .map(Outcome::Reply),
                        Operation::Layout {
                            request,
                            generation,
                        } => layout(
                            &mut input,
                            &mut output,
                            request,
                            generation,
                            &factory,
                            &mut images,
                        )
                        .map(Outcome::Page),
                    }
                    .map_err(|error| {
                        if unsafe { WaitForSingleObject(raw(&process), 150) } != 0 {
                            return error;
                        }
                        let mut code = 0;
                        unsafe {
                            GetExitCodeProcess(raw(&process), &mut code);
                        }
                        let mut details = String::new();
                        let _ = errors.by_ref().take(2048).read_to_string(&mut details);
                        io::Error::other(format!("{error} Exit 0x{code:08x}. {}", details.trim()))
                    });
                    let failed = response.is_err();
                    let _ = reply.send(response);
                    if failed {
                        break;
                    }
                }
            })?;
        Ok(Self { job, requests })
    }

    pub fn call(&self, request: Request) -> Result<Reply, String> {
        match self.exchange(Operation::Call(request))? {
            Outcome::Reply(Reply::Error(error)) => Err(error),
            Outcome::Reply(reply) => Ok(reply),
            Outcome::Page(_) => Err("Unexpected reply from Web Content Process.".into()),
        }
    }

    pub(super) fn layout(&self, request: Request, generation: u32) -> Result<Scene, String> {
        match self.exchange(Operation::Layout {
            request,
            generation,
        })? {
            Outcome::Page(page) => Ok(page),
            Outcome::Reply(_) => Err("Unexpected layout reply from Web Content Process.".into()),
        }
    }

    fn exchange(&self, operation: Operation) -> Result<Outcome, String> {
        let (reply, result) = mpsc::sync_channel(1);
        self.requests
            .send((operation, reply))
            .map_err(|_| "The Web Content Process is no longer running.")?;
        match result.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => Err(format!("Web Content Process failed. {error}")),
            Err(_) => {
                unsafe {
                    TerminateJobObject(raw(&self.job), 1);
                }
                Err("The Web Content Process did not respond.".into())
            }
        }
    }
}

struct Profile(PSID);

impl Profile {
    fn open() -> io::Result<Self> {
        let name: Vec<_> = "Browser.WebContentProcess\0".encode_utf16().collect();
        let label: Vec<_> = "Browser Web Content Process\0".encode_utf16().collect();
        let mut sid = null_mut();
        let mut result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                label.as_ptr(),
                label.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if result as u32 == 0x800700b7 {
            result = unsafe { DeriveAppContainerSidFromAppContainerName(name.as_ptr(), &mut sid) };
        }
        if result < 0 {
            return Err(io::Error::other(format!(
                "AppContainer profile HRESULT 0x{:08x}",
                result as u32
            )));
        }
        Ok(Self(sid))
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        unsafe {
            FreeSid(self.0);
        }
    }
}

struct Attributes(Vec<usize>);

impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut memory = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        check(unsafe {
            InitializeProcThreadAttributeList(memory.as_mut_ptr().cast(), count, 0, &mut bytes)
        })?;
        Ok(Self(memory))
    }

    fn pointer(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }

    fn set<T>(&mut self, id: u32, value: &T) -> io::Result<()> {
        check(unsafe {
            UpdateProcThreadAttribute(
                self.pointer(),
                0,
                id as usize,
                ptr::from_ref(value).cast::<c_void>(),
                size_of::<T>(),
                null_mut(),
                null_mut(),
            )
        })
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.pointer());
        }
    }
}

fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let (mut read, mut write) = (null_mut(), null_mut());
    check(unsafe { CreatePipe(&mut read, &mut write, &security, 0) })?;
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

fn inheritable(handle: &OwnedHandle, value: bool) -> io::Result<()> {
    check(unsafe {
        SetHandleInformation(
            raw(handle),
            HANDLE_FLAG_INHERIT,
            if value { HANDLE_FLAG_INHERIT } else { 0 },
        )
    })
}

fn job() -> io::Result<OwnedHandle> {
    let job = unsafe { CreateJobObjectW(null(), null()) };
    if job.is_null() {
        return Err(io::Error::last_os_error());
    }
    let job = unsafe { OwnedHandle::from_raw_handle(job) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    limits.BasicLimitInformation.ActiveProcessLimit = 1;
    limits.ProcessMemoryLimit = 384 * 1024 * 1024;
    check(unsafe {
        SetInformationJobObject(
            raw(&job),
            JobObjectExtendedLimitInformation,
            ptr::from_ref(&limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    })?;
    Ok(job)
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    handle.as_raw_handle()
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn read_reply(input: &mut File) -> io::Result<Reply> {
    receive(input)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            "Web Content Process closed the channel.",
        )
    })
}

fn layout(
    input: &mut File,
    output: &mut File,
    request: Request,
    generation: u32,
    factory: &Result<TextFactory, String>,
    images: &mut Vec<rendering_ffi::Image>,
) -> io::Result<Scene> {
    let factory = factory
        .as_ref()
        .map_err(|error| io::Error::other(error.clone()))?;
    let mut batch = factory.batch();
    let mut count = 0usize;
    let mut layouts = 0usize;
    let mut bytes = 0usize;
    transmit(output, &request)?;
    loop {
        match read_reply(input)? {
            Reply::Measure(text) => {
                count += 1;
                bytes += text.value.len();
                if count > 10_000 || bytes > web_engine::MAX_HTML_BYTES * 2 {
                    return Err(io::Error::other("Text measurement budget exceeded."));
                }
                let result = batch.measure_styled(
                    &text.value,
                    text.style,
                    text.width,
                    &text.links,
                    &text.runs,
                    text.wrap,
                );
                if let Ok(metrics) = &result {
                    layouts = metrics.layout as usize + 1;
                }
                transmit(output, &Request::Measured(result))?;
            }
            Reply::Page(snapshot) => {
                return prepare_snapshot(snapshot, batch, layouts, generation, images)
                    .map_err(io::Error::other);
            }
            Reply::Error(error) => return Err(io::Error::other(error)),
            _ => return Err(io::Error::other("Unexpected IPC reply during layout.")),
        }
    }
}
