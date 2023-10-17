use std::collections::HashMap;
use std::{env, io};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Result,};
use async_executor::Executor;
use crossbeam_channel::{Receiver, unbounded};
use easy_parallel::Parallel;
use futures_lite::future;
use git2::{ObjectType, Repository, TreeWalkMode, TreeWalkResult};

type Blames = Vec<(String, usize)>;

struct BlameChunk {
    author: String,
    lines: usize
}

struct CancellationToken {
    sender: Arc<Mutex<bool>>,
}

impl CancellationToken {
    fn new() -> Self { Self { sender: Arc::new(Mutex::new(false)) } }
    fn cancel(&self) { *self.sender.lock().unwrap() = true; }
    fn is_cancelled(&self) -> bool { *self.sender.lock().unwrap() }
}

fn make_blame(repo: &Repository, fname: &Path) -> Result<Blames>
{
    let fblame = repo.blame_file(&fname, None)?;
    let mut authors = HashMap::<String, usize>::new();

    for blame_chunk in fblame.iter() {
        let author = blame_chunk.final_signature().email().unwrap_or("unknown").to_string();
        let lines = blame_chunk.lines_in_hunk();
        let entry = authors.entry(author).or_insert(0);
        *entry += lines;
    }
    Ok(hm_into_vec(&authors))
}

fn get_tree<F>(repo: &Repository, updater: &mut F) -> Result<usize>
    where F: FnMut(&str)
{
    let head = repo.head()?.peel_to_tree()?;
    let mut cnt: usize = 0;

    head.walk(TreeWalkMode::PreOrder, move |path, entry| {
        if entry.kind() != Some(ObjectType::Blob) { return TreeWalkResult::Ok; }
        let mut result = path.to_owned();
        result.push_str(entry.name().expect("empty filename"));
        cnt = cnt + 1; // why does += 1 not work?
        updater(&result);
        TreeWalkResult::Ok
    })?;

    Ok(cnt)
}

fn acc_impl(blame: Blames, hm: &mut HashMap<String, usize>) {
    for blame_chunk in blame {
        let entry = hm.entry(blame_chunk.0).or_insert(0);
        *entry += blame_chunk.1;
    }
}

fn acc(rx: &Receiver<Blames>, mut count: usize) -> Result<Blames> {
    let mut agg = HashMap::<String, usize>::new();
    while count > 0 {
        acc_impl(rx.recv()?, &mut agg);
        count -= 1;
    }
    Ok(hm_into_vec(&agg))
}

fn hm_into_vec(authors: &HashMap<String, usize>) -> Blames {
    let mut blames: Blames = authors.iter()
        .map(|(x,y)| { (x.to_owned(), y.to_owned())} )
        .collect();
    blames.sort_by(|lhs, rhs| rhs.1.cmp(&lhs.1));
    blames
}

fn single_threaded(path: &str) -> Result<Blames> {
    let repo = Repository::open(path)?;
    let mut files = Vec::new();
    let mut result = HashMap::new();
    let _ = get_tree(&repo, &mut |path: &str| {
        files.push(path.to_owned());
    })?;
    for f in files {
        let blame = make_blame(&repo, Path::new(&f))?;
        acc_impl(blame, &mut result);
    }
    Ok(hm_into_vec(&result))
}

fn multi_threaded(path: &str) ->  Result<Vec<Blames>>{
    anyhow::bail!("not implemented")
    //
    // let executor = Executor::new();
    // let (to_pool, for_pool) = unbounded();
    // let (to_acc, for_acc) = unbounded();
    //
    // let repo = Arc::new(Mutex::new(Repository::open(".")));
    // let update = move |path: &str| {
    //     to_pool.send(path.to_owned()).unwrap(); // FIXME: unwrap()
    // };
    //
    // let ct = CancellationToken::new();
    //
    // let mut tasks = Vec::new();
    //
    // for _ in 0..4 {
    //     let task = executor.spawn(
    //         async {}
    //     );
    //     tasks.push(task);
    // }
    // let p = Parallel::new()
    //     // Run four executor threads.
    //     .each(0..4, |_| future::block_on(executor.run(
    //         async {
    //             while !ct.is_cancelled() {
    //                 let m = for_pool.recv().unwrap();
    //                 let res = make_blame(&repo.lock().unwrap(), Path::new(&m)).unwrap();
    //                 to_acc.send(res).unwrap();
    //                 ()
    //             }
    //         })));
    // p.run();
    //
    // let cnt = get_tree(&repo.lock().unwrap(), update)?;
    //
    // let mut vec = acc(&for_acc, cnt)?;
    // ct.cancel();

    // let mut vec : Vec<(&String, &usize)> = authors.iter().collect();
}

fn main() -> Result<()> {
    // TODO: cargo add clap
    // --single
    // --parallel
    // --path
    // --top n

    let blames = single_threaded(".")?;
    let mut wtr = csv::Writer::from_writer(io::stdout());
    for b in blames {
        wtr.serialize(b)?;
    }
    Ok(())
}
