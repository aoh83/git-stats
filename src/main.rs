use std::collections::HashMap;
use std::error::Error;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::available_parallelism;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::bounded;
use git2::{ObjectType, Repository, TreeWalkMode, TreeWalkResult};
use serde::Serialize;
use tokio::runtime::Runtime;

#[derive(Clone)]
struct CancellationToken {
    sender: Arc<Mutex<bool>>,
}

impl CancellationToken {
    fn new() -> Self { Self { sender: Arc::new(Mutex::new(false)) } }
    fn cancel(&self) { *self.sender.lock().unwrap() = true; }
    fn is_cancelled(&self) -> bool { *self.sender.lock().unwrap() }
}

async fn retry<F, E, V>(mut f: F, mut attempts: u8, interval: Duration) -> Result<V, E>
    where
        E: Error,
        F: FnMut() -> Result<V, E>,
{
    loop {
        match f() {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempts == 0 {
                    return Err(e);
                }
                attempts -= 1;
                tokio::time::sleep(interval).await;
            }
        }
    }
}

#[derive(Debug, Serialize, Clone)]
struct Blame {
    author: String,
    lines: usize,
}

type Blames = Vec<Blame>;
type HBlames = HashMap<String, usize>;

#[derive(Clone, Debug)]
enum BlameMessage {
    Blame(HBlames),
    Count(usize)
}
fn blame_file(repo: &Repository, fname: &Path) -> Result<HBlames>
{
    let fblame = repo.blame_file(fname, None)?;
    let mut authors = HashMap::<String, usize>::new();

    for blame_chunk in fblame.iter() {
        let author = blame_chunk.final_signature().email().unwrap_or("unknown").to_string();
        let lines = blame_chunk.lines_in_hunk();
        let entry = authors.entry(author).or_insert(0);
        *entry += lines;
    }
    Ok(authors)
}

fn get_tree<F>(repo: &Repository, updater: &mut F) -> Result<usize>
    where F: FnMut(&str) -> Result<()>
{
    let head = repo.head()?.peel_to_tree()?;
    let mut cnt: usize = 0;

    head.walk(TreeWalkMode::PreOrder, move |path, entry| {
        if entry.kind() != Some(ObjectType::Blob) { return TreeWalkResult::Ok; }
        let mut result = path.to_owned();
        result.push_str(entry.name().expect("empty filename"));
        cnt = cnt + 1; // why does += 1 not work?
        let result = updater(&result);
        if result.is_err() { TreeWalkResult::Abort } else { TreeWalkResult::Ok }
    })?;

    Ok(cnt)
}

fn blame_fold(mut hm: HBlames,  blame: HBlames) -> HBlames {
    for blame_chunk in blame {
        let entry = hm.entry(blame_chunk.0).or_insert(0);
        *entry += blame_chunk.1;
    }
    hm
}

fn blame_acc(hm: &mut HashMap<String, usize>,  blame: HBlames)  {
    for blame_chunk in blame {
        let entry = hm.entry(blame_chunk.0).or_insert(0);
        *entry += blame_chunk.1;
    }
}

fn hm_into_vec(authors: &HBlames) -> Blames {
    let mut blames: Blames = authors.iter()
        .map(|(x, y)| { Blame {author: x.to_owned(), lines: y.to_owned()} })
        .collect();
    blames.sort_by(|lhs, rhs| rhs.lines.cmp(&lhs.lines));
    blames
}

fn single_threaded(path: &str) -> Result<Blames> {
    let repo = Repository::open(path)?;
    let mut files = Vec::new();
    let _ = get_tree(&repo, &mut |path: &str| {
        files.push(path.to_owned());
        Ok(())
    })?;
    let result = files.iter()
        .map(|f| { blame_file(&repo, Path::new(&f)).expect("unblamable") })
        .fold(HashMap::new(), &blame_fold);
    Ok(hm_into_vec(&result))
}


fn multi_threaded(path: &str, workers: usize) ->  Result<Blames> {
    let rt = Runtime::new().unwrap();
    let (to_pool, for_pool) = bounded(100);
    let (to_acc, for_acc) = bounded(100);
    let (to_print, for_print) = bounded(1);
    let ct = CancellationToken::new();

    {
        let to_acc = to_acc.clone();
        let to_pool = to_pool.clone();
        let path = path.to_owned();
        rt.spawn(async move {
            let repo = Repository::open(path).unwrap();
            let mut update = |path: &str| -> Result<()> {
                to_pool.send(path.to_owned())?;
                Ok(())
            };
            let cnt = get_tree(&repo, &mut update).unwrap();
            to_acc.send(BlameMessage::Count(cnt)).unwrap();
        });
    }

    for _ in 0..workers {
        let for_pool = for_pool.clone();
        let ct = ct.clone();
        let path = path.to_owned();
        let to_acc = to_acc.clone();
        rt.spawn(async move {
            let repo = Repository::open(path).unwrap();
            while !ct.is_cancelled() {
                let m = for_pool.recv().unwrap();
                let res = blame_file(&repo, Path::new(&m)).unwrap();
                let bm = BlameMessage::Blame(res);
                let retry_result = retry(|| { to_acc.send(bm.clone()) }, 20, Duration::from_millis(1)).await;
                if retry_result.is_err() {
                    println!("closing worker, retries exceeded\n");
                }
            }
        });
    }

    {
        let mut hm = HashMap::new();
        rt.spawn(async move {
            let mut count = 0;
            let mut c = None;
            while c.is_none() || count < c.unwrap() {
                let msg = for_acc.recv().unwrap();
                match msg {
                    BlameMessage::Count(i) => c = Some(i),
                    BlameMessage::Blame(b) => blame_acc(&mut hm, b)
                }
                count += 1;
            }
            ct.cancel();
            to_print.send(hm_into_vec(&hm)).unwrap();
        });
    }
    rt.block_on(async {});
    Ok(for_print.recv().unwrap())
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    single: bool,

    #[arg(short, long, default_value_t = 0)]
    workers: usize,

    #[arg(short, long, default_value = ".")]
    path: String,

    #[arg(short, long, default_value_t = 0)]
    top: usize,

}

fn main() -> Result<()> {
    let mut args = Args::parse();

    if args.workers == 0 {
        args.workers = available_parallelism().unwrap().get();
    }
    println!("using {} workers", args.workers);

    let blames = if args.single {
        single_threaded(&args.path)?
    } else {
        multi_threaded(&args.path, args.workers)?
    };
    let mut wtr = csv::Writer::from_writer(io::stdout());
    if args.top == 0 {
        for b in blames {
            wtr.serialize(b)?;
        }
    } else {
        for i in 0..args.top {
            wtr.serialize(blames.get(i))?;
        }
    }
    Ok(())
}
