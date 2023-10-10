use anyhow::Result;
use git2::{Repository,  ObjectType, TreeWalkMode, TreeWalkResult};
use std::collections::HashMap;
use std::env;
use std::path::Path;

fn make_blame(repo: &Repository, fname :&Path, authors: &mut HashMap<String, usize>) -> Result<()>{
        let fblame = repo.blame_file(&fname, None)?;

        for blame_chunk in fblame.iter() {
            let author = blame_chunk.final_signature().email().unwrap_or("unknown").to_string();
            let lines = blame_chunk.lines_in_hunk();

            let entry = authors.entry(author).or_insert(0);
            *entry += lines;
        }
        Ok(())
}

fn get_tree(repo: &Repository) -> Result<HashMap<String, usize>> {
    let head = repo.head()?.peel_to_tree()?;
    let mut authors = HashMap::<String, usize>::new();
    
    head.walk(TreeWalkMode::PreOrder, |path, entry| {
        if entry.kind() != Some(ObjectType::Blob) { return TreeWalkResult::Ok; }
        let mut result = path.to_owned();
        result.push_str(entry.name().expect("empty filename"));
        make_blame(repo, Path::new(&result), &mut authors).unwrap(); // FIXME: remove unwrap
        TreeWalkResult::Ok
    })?;
    
    Ok(authors)
}

fn main() -> Result<()> {
    // TODO: cargo add clap
    let args: Vec<String> = env::args().collect();
    assert!(args.len() == 2);

    let repo = Repository::open(&args[1])?;
    let authors = get_tree(&repo)?;
    
    let mut vec : Vec<(&String, &usize)> = authors.iter().collect();
    vec.sort_by(|lhs, rhs| rhs.1.cmp(lhs.1));
    
    println!("{:?}", vec);
    Ok(())
}
