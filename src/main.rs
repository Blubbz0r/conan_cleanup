extern crate clap;
extern crate ini;
extern crate serde_json;
extern crate walkdir;

use ini::Ini;
use walkdir::WalkDir;

use std::collections::HashMap;
use std::fmt;
use std::process::Command;

fn main() {
    let args = clap::App::new("conan_cleanup")
        .version("0.1")
        .about("Aids in removing unused conan packages from the local cache")
        .arg(clap::Arg::with_name("root_path")
            .help("Path to the directory containing all projects that use conan. It is recursively parsed for conaninfo.txt files to know which packages are actively used.")
            .required(true))
        .arg(clap::Arg::with_name("force")
            .short("f")
            .help("Force complete removal of unused packages without requiring manual approval.")
            .takes_value(false))
        .get_matches();

    let root_path = args.value_of("root_path").unwrap();
    let packages_in_use = find_packages_in_use(root_path);

    let json_path = temp_json_file_path();
    Command::new("conan")
        .args(&["search", "-j", &json_path.to_string_lossy()])
        .output()
        .unwrap_or_else(|err| {
            eprintln!("'conan search' failed: {}", err);
            std::process::exit(-1)
        });

    let recipe_ids = match parse_recipe_ids(&json_path) {
        Ok(ids) => ids,
        Err(err) => {
            eprintln!("Failed to parse used recipe IDs: {}", err);
            std::process::exit(-1)
        }
    };
    let mut recipes_and_packages = HashMap::new();
    for recipe_id in &recipe_ids {
        Command::new("conan")
            .args(&["search", "-j", &json_path.to_string_lossy(), recipe_id])
            .output()
            .unwrap_or_else(|err| {
                eprintln!("'conan search' failed: {}", err);
                std::process::exit(-1)
            });

        let package_ids = match parse_package_ids(&json_path) {
            Ok(ids) => ids,
            Err(err) => {
                eprintln!(
                    "Failed to parse packages IDs available in local cache: {}",
                    err
                );
                std::process::exit(-1)
            }
        };
        recipes_and_packages.insert(recipe_id, package_ids);
    }

    let mut packages_to_remove = HashMap::new();
    for (recipe_id, package_ids) in &recipes_and_packages {
        let mut package_ids_to_remove = Vec::new();
        for package_id in package_ids {
            if !packages_in_use.contains(package_id) {
                package_ids_to_remove.push(package_id);
            }
        }

        if !package_ids_to_remove.is_empty() {
            packages_to_remove.insert(recipe_id, package_ids_to_remove);
        }
    }

    let force = args.is_present("force");
    if !packages_to_remove.is_empty() {
        println!("Packages to remove:");
        for (recipe_id, package_ids) in &packages_to_remove {
            println!("{}", recipe_id);
            for package_id in package_ids {
                println!("  {}", package_id);
            }
        }

        if !force {
            println!("Do you want to remove the packages listed above? (yes/no)");
        }

        if force || get_yes_or_no() {
            for (recipe_id, package_ids) in &packages_to_remove {
                for package_id in package_ids {
                    Command::new("conan")
                        .args(&["remove", recipe_id, "-p", package_id, "-f"])
                        .output()
                        .unwrap_or_else(|err| {
                            eprintln!("'conan remove' failed: {}", err);
                            std::process::exit(-1)
                        });
                }
            }
        }
    } else {
        println!("No unused packages found.");
    }

    if !force {
        println!("Do you want to remove recipes that no longer have any packages? (yes/no)");
    }

    if force || get_yes_or_no() {
        for recipe_id in &recipe_ids {
            Command::new("conan")
                .args(&["search", "-j", &json_path.to_string_lossy(), recipe_id])
                .output()
                .unwrap_or_else(|err| {
                    eprintln!("'conan search' failed: {}", err);
                    std::process::exit(-1)
                });

            let package_ids = match parse_package_ids(&json_path) {
                Ok(ids) => ids,
                Err(err) => {
                    eprintln!(
                        "Failed to parse package IDs of recipe '{}': {}",
                        recipe_id, err
                    );
                    std::process::exit(-1)
                }
            };

            if package_ids.is_empty() {
                println!(
                    "Removing recipe '{}' since it has no packages left",
                    recipe_id
                );

                let remove_command = Command::new("conan")
                    .args(&["remove", recipe_id, "-f"])
                    .output()
                    .unwrap_or_else(|err| {
                        eprintln!("'conan remove' failed: {}", err);
                        std::process::exit(-1)
                    });

                if !remove_command.status.success() {
                    if !remove_command.stderr.is_empty() {
                        eprintln!(
                            "{}",
                            String::from_utf8_lossy(remove_command.stderr.as_slice())
                        );
                    }
                    if !remove_command.stdout.is_empty() {
                        eprintln!(
                            "{}",
                            String::from_utf8_lossy(remove_command.stdout.as_slice())
                        );
                    }
                }
            }
        }
    }

    if let Err(err) = std::fs::remove_file(&json_path) {
        eprintln!(
            "Failed to remove temporary file '{}': {}",
            json_path.display(),
            err
        );
        eprintln!("Please remove the file manually.");
    }
}

fn parse_recipe_ids(result_file_path: &std::path::Path) -> Result<Vec<String>, ConanJsonError> {
    let file_content = std::fs::read_to_string(result_file_path)?;
    let json: serde_json::Value = serde_json::from_str(&file_content)?;
    let results = json["results"].as_array().ok_or_else(|| {
        ConanJsonError::FormatError("Missing top-level 'results' array".to_owned())
    })?;
    let result_object = &results[0].as_object().ok_or_else(|| {
        ConanJsonError::FormatError("'results' array is missing its root object".to_owned())
    })?;
    let items = result_object["items"].as_array().ok_or_else(|| {
        ConanJsonError::FormatError(
            "Root object of 'results' array is missing the 'items' array".to_owned(),
        )
    })?;
    let mut recipe_ids = Vec::new();
    for item in items {
        let recipe_object = item["recipe"].as_object().ok_or_else(|| {
            ConanJsonError::FormatError("'items' array is missing the 'recipe' object".to_owned())
        })?;
        let id = recipe_object["id"].as_str().ok_or_else(|| {
            ConanJsonError::FormatError("'recipe' object is missing the 'id' string".to_owned())
        })?;
        recipe_ids.push(id.to_owned());
    }
    Ok(recipe_ids)
}

fn parse_package_ids(result_file_path: &std::path::Path) -> Result<Vec<String>, ConanJsonError> {
    let file_content = std::fs::read_to_string(result_file_path)?;
    let json: serde_json::Value = serde_json::from_str(&file_content)?;
    let results = json["results"].as_array().ok_or_else(|| {
        ConanJsonError::FormatError("Missing top-level 'results' array".to_owned())
    })?;
    let result_object = &results[0].as_object().ok_or_else(|| {
        ConanJsonError::FormatError("'results' array is missing its root object".to_owned())
    })?;
    let items = result_object["items"].as_array().ok_or_else(|| {
        ConanJsonError::FormatError(
            "Root object of 'results' array is missing the 'items' array".to_owned(),
        )
    })?;
    let items_object = &items[0]
        .as_object()
        .ok_or_else(|| ConanJsonError::FormatError("'items' array has no objects".to_owned()))?;

    let mut package_ids: Vec<String> = Vec::new();

    if items_object.contains_key("packages") {
        let packages = items_object["packages"].as_array().ok_or_else(|| {
            ConanJsonError::FormatError("First 'items' object has no 'packages' array".to_owned())
        })?;

        for package in packages {
            let id = package["id"].as_str().ok_or_else(|| {
                ConanJsonError::FormatError("'package' is missing an 'id' string".to_owned())
            })?;
            package_ids.push(id.to_owned());
        }
    }

    Ok(package_ids)
}

fn find_packages_in_use(root_path: &str) -> Vec<String> {
    let mut packages_in_use = Vec::new();
    for entry in WalkDir::new(root_path)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_name() == "conaninfo.txt" {
            let packages = match parse_required_packages(entry.path()) {
                Ok(packages) => packages,
                Err(ref err) => {
                    eprintln!("Failed to parse '{}': {}", entry.path().display(), err);
                    continue;
                }
            };

            packages_in_use.extend(packages);
        }
    }

    packages_in_use.sort();
    packages_in_use.dedup();
    packages_in_use
}

#[derive(Debug)]
enum ConanIniError {
    Ini(ini::ini::Error),
    MissingSection(String),
}

impl fmt::Display for ConanIniError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ConanIniError::Ini(ref err) => err.fmt(f),
            ConanIniError::MissingSection(ref section) => {
                write!(f, "Section '{}' is missing", section)
            }
        }
    }
}

impl std::error::Error for ConanIniError {
    fn description(&self) -> &str {
        match *self {
            ConanIniError::Ini(ref err) => err.description(),
            ConanIniError::MissingSection(_) => "missing section",
        }
    }
}

impl From<ini::ini::Error> for ConanIniError {
    fn from(err: ini::ini::Error) -> ConanIniError {
        ConanIniError::Ini(err)
    }
}

fn parse_required_packages<P: AsRef<std::path::Path>>(
    file_path: P,
) -> Result<Vec<String>, ConanIniError> {
    let conan_info = Ini::load_from_file(file_path)?;
    let full_requires = conan_info
        .section(Some("full_requires".to_owned()))
        .ok_or_else(|| ConanIniError::MissingSection("full_requires".to_owned()))?;

    let mut required_packages = Vec::new();
    for (_, value) in full_requires {
        required_packages.push(value.to_owned());
    }
    Ok(required_packages)
}

#[derive(Debug)]
enum ConanJsonError {
    Io(std::io::Error),
    Json(serde_json::Error),
    FormatError(String),
}

impl fmt::Display for ConanJsonError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            ConanJsonError::Io(ref err) => err.fmt(f),
            ConanJsonError::Json(ref err) => err.fmt(f),
            ConanJsonError::FormatError(ref err) => write!(
                f,
                "Unexpected JSON format (conan might have changed its output format): {}",
                err
            ),
        }
    }
}

impl std::error::Error for ConanJsonError {
    fn description(&self) -> &str {
        match *self {
            ConanJsonError::Io(ref err) => err.description(),
            ConanJsonError::Json(ref err) => err.description(),
            ConanJsonError::FormatError(_) => "unexpected JSON format",
        }
    }
}

impl From<std::io::Error> for ConanJsonError {
    fn from(err: std::io::Error) -> ConanJsonError {
        ConanJsonError::Io(err)
    }
}

impl From<serde_json::Error> for ConanJsonError {
    fn from(err: serde_json::Error) -> ConanJsonError {
        ConanJsonError::Json(err)
    }
}

fn temp_json_file_path() -> std::path::PathBuf {
    let mut temp_dir = std::env::temp_dir();
    temp_dir.push("conan_search_result");
    temp_dir.set_extension("json");
    temp_dir
}

fn get_yes_or_no() -> bool {
    loop {
        let mut answer = String::new();
        if let Err(err) = std::io::stdin().read_line(&mut answer) {
            eprintln!("Failed to read answer from stdin: {}", err);
            std::process::exit(-1)
        }

        match answer.trim() {
            "Yes" | "yes" | "y" | "Y" => return true,
            "No" | "no" | "n" | "N" => return false,
            _ => println!("yes/no?"),
        }
    }
}
