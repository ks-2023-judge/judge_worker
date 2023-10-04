use bytes::{Buf, BytesMut};
use nix::errno::Errno;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc::Sender,
};

use crate::{protocol::*, types::*, Config, ControlMsg};

pub struct Core {
    tx: Sender<ControlMsg>,
    config: Config,

    recv_buf: BytesMut,
    send_buf: BytesMut,
}

impl Core {
    pub fn new(config: Config, tx: Sender<ControlMsg>) -> Self {
        Self {
            config,
            tx,
            recv_buf: BytesMut::with_capacity(8192),
            send_buf: BytesMut::with_capacity(8192),
        }
    }

    pub async fn run(mut self) {
        let mut sock = match tokio::net::TcpStream::connect(&self.config.server_addr).await {
            Ok(sock) => sock,
            Err(e) => {
                eprintln!("Failed to connect to server: {:?}", e);
                return;
            }
        };

        let register_msg = MsgRegister {
            is_precise_server: self.config.is_precise,
        };

        let msg_body: MessageBody = register_msg.into();
        msg_body.encode(&mut self.send_buf);

        sock.write_all_buf(&mut self.send_buf).await.unwrap();

        self.tx
            .send(ControlMsg::Connect(self.config.affintiy))
            .await
            .unwrap();

        loop {
            match sock.read_buf(&mut self.recv_buf).await {
                Ok(0) | Err(_) => break,
                _ => (),
            }

            if let Some(Ok(msg)) = MessageBody::decode_buf(&mut self.recv_buf)
                .map(|msg| msg.try_into() as Result<Message, _>)
            {
                if self.process(&mut sock, msg).await.is_err() {
                    break;
                }
            };
        }
    }
    async fn process(&mut self, sock: &mut TcpStream, msg: Message) -> Result<(), ()> {
        match msg {
            Message::SetTask(msg) => {
                let MsgSetTask {
                    code,
                    lang,
                    submission_id,
                    testcase_id,

                    input,
                    expect_output,
                    time_limit,
                    memory_limit,
                    ..
                } = msg;

                self.tx
                    .send(ControlMsg::ReceiveTask(lang.into(), code.clone()))
                    .await
                    .map_err(|_| ())?;

                let msg = Message::SetTaskAck(MsgSetTaskAck {
                    submission_id,
                    testcase_id,
                });
                let msg_body: MessageBody = msg.into();

                msg_body.encode(&mut self.send_buf);
                sock.write_all_buf(&mut self.send_buf)
                    .await
                    .map_err(|_| ())?;

                let result = MsgResult {
                    submission_id,
                    testcase_id,
                    result: TestCaseJudgeResultInner::NotYetDone,
                    output_compile: String::new(),
                    output_run: String::new(),
                    result_extra: String::new(),
                    time_used: 0,
                    memory_used: 0,
                    judge_server_id: format!(
                        "{}:{}",
                        self.config.server_addr, self.config.affintiy
                    ),
                };

                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(time_limit + 10),
                    self.run_process(
                        result,
                        lang,
                        code,
                        input,
                        expect_output,
                        time_limit,
                        memory_limit,
                    ),
                )
                .await;

                println!("run result: {:?}", result,);

                match result {
                    Ok(Ok(result)) => {
                        let msg = if matches!(result.result, TestCaseJudgeResultInner::Accepted) {
                            Message::ResultSuccess(MsgResultSuccess(result))
                        } else {
                            Message::ResultFailed(MsgResultFailed(result))
                        };

                        let msg_body: MessageBody = msg.into();
                        println!("send -> {:?}", msg_body);

                        msg_body.encode(&mut self.send_buf);

                        println!("buf -> {:?}", self.send_buf);

                        sock.write_all_buf(&mut self.send_buf)
                            .await
                            .map_err(|_| ())?;
                    }
                    _ => {
                        let msg = Message::ResultFailed(MsgResultFailed(result.unwrap().unwrap()));
                        let msg_body: MessageBody = msg.into();
                        msg_body.encode(&mut self.send_buf);

                        sock.write_all_buf(&mut self.send_buf)
                            .await
                            .map_err(|_| ())?;
                    }
                };

                Ok(())
            }
            _ => todo!(),
        }
    }

    async fn run_process(
        &self,
        mut result_form: MsgResult,
        lang: SubmissionLanguage,
        code: String,
        input: String,
        expect_output: String,
        max_time: u64,
        max_memory: u64,
    ) -> Result<MsgResult, Errno> {
        // result_form.result = TestCaseJudgeResultInner::CompileFailed;

        // preparing file
        let dir_target = format!(
            "{}/workspace-{}",
            std::env::current_dir().unwrap().to_str().unwrap(),
            self.config.affintiy
        );
        if std::path::Path::new(&dir_target).exists() {
            std::fs::remove_dir_all(&dir_target).unwrap();
        }

        std::fs::create_dir(&dir_target).unwrap();

        let file_target = match lang {
            SubmissionLanguage::C => "Answer.c",
            SubmissionLanguage::Cpp => "Answer.cpp",
            SubmissionLanguage::Java => "Answer.java",
            SubmissionLanguage::Python => "Answer.py",
            SubmissionLanguage::Rust => "Answer.rs",
            SubmissionLanguage::Javascript => "Answer.js",
        };
        let file_target = format!("{}/{}", dir_target, file_target);

        std::fs::write(file_target, code).unwrap();

        // compile with docker (docker run -v $workspace-{affinity}/:/judge -w /judge {lang}_compile)
        let mut cmd_compile = tokio::process::Command::new("docker")
            .args([
                "run",
                "-v",
                &format!("{}:/judge", dir_target),
                "-w",
                "/judge",
                "--rm",
                &format!("--cpuset-cpus={}", self.config.affintiy),
                &format!("{}_compile", <String>::from(lang)),
            ])
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let mut cmd_compile_stdout = cmd_compile.stdout.take().unwrap();
        let mut cmd_compile_stderr = cmd_compile.stderr.take().unwrap();

        let mut output_buf = BytesMut::with_capacity(2048);
        let mut error_buf = BytesMut::with_capacity(2048);

        loop {
            tokio::select! {
                recv_len = cmd_compile_stdout.read_buf(&mut output_buf) => {
                    match recv_len {
                        Ok(0) | Err(_) => break,
                        _ => (),
                    }

                    let mut cnt = output_buf.len();

                    while cnt > 0 {
                        match std::str::from_utf8(&output_buf[0..cnt]) {
                            Ok(s) => {
                                result_form.output_compile.push_str(s);
                                output_buf.advance(cnt);
                                cnt = output_buf.len();
                            }
                            Err(_) => {
                                cnt -= 1;
                            }
                        }
                    }
                }
                recv_len = cmd_compile_stderr.read_buf(&mut error_buf) => {
                     match recv_len {
                        Ok(0) | Err(_) => break,
                        _ => (),
                    }

                    let mut cnt = error_buf.len();

                    while cnt > 0 {
                        match std::str::from_utf8(&error_buf[0..cnt]) {
                            Ok(s) => {
                                result_form.output_compile.push_str(s);
                                error_buf.advance(cnt);
                                cnt = output_buf.len();
                            }
                            Err(_) => {
                                cnt -= 1;
                            }
                        }
                    }
                }
                // output = cmd_compile.wait() => {
                //     match output {
                //         Err(e) => {
                //             eprintln!("Failed to compile: {:?}", e);

                //             return Ok(result_form);
                //         }
                //         Ok(status) => {
                //             if !status.success() {
                //                 result_form.result = TestCaseJudgeResultInner::CompileFailed;
                //                return Ok(result_form);
                //             }
                //         }
                //     }
                // }
            }
        }

        let mut max_try = 5;
        loop {
            match cmd_compile.try_wait() {
                Err(_) | Ok(None) => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                    if max_try <= 0 {
                        break;
                    }
                    max_try -= 1;
                }
                Ok(Some(v)) => {
                    if !v.success() {
                        result_form.result = TestCaseJudgeResultInner::CompileFailed;
                        return Ok(result_form);
                    }

                    break;
                }
            }
        }

        output_buf.clear();
        error_buf.clear();

        let mut cmd_run = tokio::process::Command::new("docker")
            .args([
                "run",
                "-v",
                &format!("{}:/judge", dir_target),
                "-w",
                "/judge",
                "--rm",
                "-m",
                "2G",
                "--memory-reservation",
                max_memory.to_string().as_str(),
                &format!("--cpuset-cpus={}", self.config.affintiy),
                &format!("{}_run", <String>::from(lang)),
            ])
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();

        let mut run_stdin = cmd_run.stdin.take().unwrap();
        let mut run_stdout = cmd_run.stdout.take().unwrap();
        let mut run_stderr = cmd_run.stderr.take().unwrap();

        let start_time = std::time::Instant::now();

        run_stdin.write_all(input.as_bytes()).await;
        run_stdin.flush().await;

        let mut max_time_limit = tokio::time::interval(std::time::Duration::from_millis(max(
            max_time, 30_000,
        )
            as u64));

        max_time_limit.tick().await;

        loop {
            tokio::select! {
                recv_len = run_stdout.read_buf(&mut output_buf) => {
                    match recv_len {
                        Ok(0) | Err(_) => break,
                        _ => ()
                    };

                    let mut cnt = output_buf.len();
                    while cnt > 0 {
                        match std::str::from_utf8(&output_buf[0..cnt]) {
                            Ok(s) => {
                                result_form.output_run.push_str(s);
                                output_buf.advance(cnt);
                                cnt = output_buf.len();
                            }
                            Err(_) => {
                                cnt -= 1;
                            }
                        }
                    }

                    if result_form.output_run.len() > max(expect_output.len() + 256, 16384) {
                        result_form.result = TestCaseJudgeResultInner::OutputLimitExceeded;
                        return Ok(result_form);
                    }
                }
                recv_len = run_stderr.read_buf(&mut error_buf) => {
                    match recv_len {
                        Ok(0) | Err(_) => break,
                        _ => ()
                    };

                    let mut cnt = error_buf.len();
                    while cnt > 0 {
                        match std::str::from_utf8(&error_buf[0..cnt]) {
                            Ok(s) => {
                                result_form.output_run.push_str(s);
                                error_buf.advance(cnt);
                                cnt = error_buf.len();
                            }
                            Err(_) => {
                                cnt -= 1;
                            }
                        }
                    }


                    if result_form.output_run.len() > max(expect_output.len() + 256, 16384) {
                        result_form.result = TestCaseJudgeResultInner::OutputLimitExceeded;
                        return Ok(result_form);
                    }
                }
                _ = max_time_limit.tick() => {
                    result_form.result = TestCaseJudgeResultInner::TimeLimitExceeded;
                    return Ok(result_form);
                }
            }
        }

        let stop = std::time::Instant::now();
        let diff = stop.duration_since(start_time);

        result_form.time_used = diff.as_millis() as u64;

        if diff.as_millis() > max_time as u128 {
            result_form.result = TestCaseJudgeResultInner::TimeLimitExceeded;
            return Ok(result_form);
        }

        let mut max_try = 5;
        loop {
            match cmd_run.try_wait() {
                Err(_) | Ok(None) => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                    if max_try <= 0 {
                        break;
                    }
                    max_try -= 1;
                }
                Ok(Some(v)) => {
                    println!("exit result = {:?}", v);
                    if !v.success() {
                        result_form.result = TestCaseJudgeResultInner::RuntimeError;
                        return Ok(result_form);
                    }

                    break;
                }
            }
        }

        if result_form.output_run != expect_output {
            result_form.result = TestCaseJudgeResultInner::WrongAnswer;
            return Ok(result_form);
        }

        result_form.result = TestCaseJudgeResultInner::Accepted;

        // limit time with rlimit - cpu. the secs is from max_time

        // let core_id = self.config.affinity;

        // match unsafe { nix::unistd::fork() } {
        //     Ok(ForkResult::Parent { child: _, .. }) => {
        //         // parent
        //         let _status = 0;
        //     }
        //     Ok(ForkResult::Child) => {
        //         setrlimit(
        //             Resource::RLIMIT_CPU,
        //             (_max_time as f64 * self.config.cpu_scale).ceil() as rlim_t,
        //             (_max_time as f64 * self.config.cpu_scale).ceil() as rlim_t + 10,
        //         )?;

        //         unsafe {
        //             use caps::{CapSet, Capability};

        //             caps::has_cap(None, CapSet::Permitted, Capability::CAP_SYS_RESOURCE).and_then(
        //                 |_| caps::drop(None, CapSet::Effective, Capability::CAP_SYS_RESOURCE),
        //             );
        //             caps::has_cap(None, CapSet::Permitted, Capability::CAP_SETPCAP)

        //             let cpuset = [core_id as libc::cpu_set_t];
        //             libc::sched_setaffinity(0, 1, &cpuset);

        //             let args = [CStr::from_ptr(std::ptr::null()); 0];

        //             execve(
        //                 CStr::from_bytes_with_nul_unchecked(b"/bin/sleep\0"),
        //                 &args,
        //                 &args,
        //             );
        //         }
        //     }

        //     _ => {}
        // }

        Ok(result_form)
    }
}

fn max<O>(a: O, b: O) -> O
where
    O: PartialOrd,
{
    if a < b {
        b
    } else {
        a
    }
}
