use std::io::Read;

pub fn run(args: &[String]) {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("failed to read stdin");

    let val: serde_json::Value = match serde_json::from_str(&input) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("JSON parse error: {e}");
            std::process::exit(1);
        }
    };

    match args.first().map(String::as_str) {
        Some("get") => {
            let path = args.get(1).map(String::as_str).unwrap_or("");
            let fallback_path = args.get(2).map(String::as_str);
            cmd_get(&val, path, fallback_path);
        }
        Some("lines") => {
            let path = args.get(1).map(String::as_str).unwrap_or("");
            cmd_lines(&val, path);
        }
        Some("pairs") => {
            let path = args.get(1).map(String::as_str).unwrap_or("");
            cmd_pairs(&val, path);
        }
        Some("env-merge") => {
            let json_path = args.get(1).map(String::as_str).unwrap_or("");
            let env_file = args.get(2).map(String::as_str).unwrap_or("");
            cmd_env_merge(&val, json_path, env_file);
        }
        _ => {
            eprintln!("Usage: acc-agent json <get|lines|pairs|env-merge> [path] [...]");
            std::process::exit(1);
        }
    }
}

fn nav<'a>(val: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let path = path.strip_prefix('.').unwrap_or(path);
    if path.is_empty() {
        return Some(val);
    }
    let mut cur = val;
    for key in path.split('.') {
        cur = match cur {
            serde_json::Value::Object(m) => m.get(key)?,
            serde_json::Value::Array(a) => {
                let idx: usize = key.parse().ok()?;
                a.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

fn print_scalar(v: &serde_json::Value) {
    match v {
        serde_json::Value::String(s) => println!("{s}"),
        serde_json::Value::Null => {}
        other => println!("{other}"),
    }
}

fn cmd_get(val: &serde_json::Value, path: &str, fallback: Option<&str>) {
    if let Some(v) = nav(val, path) {
        if !v.is_null() {
            print_scalar(v);
            return;
        }
    }
    if let Some(fb) = fallback {
        if let Some(v) = nav(val, fb) {
            print_scalar(v);
        }
    }
}

fn cmd_lines(val: &serde_json::Value, path: &str) {
    let target = nav(val, path).unwrap_or(&serde_json::Value::Null);
    if let serde_json::Value::Array(arr) = target {
        for item in arr {
            print_scalar(item);
        }
    }
}

fn cmd_pairs(val: &serde_json::Value, path: &str) {
    let target = nav(val, path).unwrap_or(&serde_json::Value::Null);
    if let serde_json::Value::Object(map) = target {
        for (k, v) in map {
            if let serde_json::Value::String(s) = v {
                println!("{k}={s}");
            }
        }
    }
}

fn cmd_env_merge(val: &serde_json::Value, json_path: &str, env_file: &str) {
    let target = nav(val, json_path).unwrap_or(&serde_json::Value::Null);
    let serde_json::Value::Object(map) = target else {
        eprintln!("env-merge: path '{json_path}' is not an object");
        std::process::exit(1);
    };

    let existing = std::fs::read_to_string(env_file).unwrap_or_default();
    let mut lines: Vec<String> = existing.lines().map(String::from).collect();

    for (k, v) in map {
        let serde_json::Value::String(s) = v else {
            continue; // skip non-string values
        };
        let new_line = format!("{k}={s}");
        let prefix = format!("{k}=");
        if let Some(pos) = lines.iter().position(|l| l.starts_with(&prefix)) {
            lines[pos] = new_line;
        } else {
            lines.push(new_line);
        }
    }

    let content = lines.join("\n") + "\n";
    if env_file == "-" {
        print!("{content}");
    } else {
        std::fs::write(env_file, content).expect("failed to write env file");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_nav_simple() {
        let v = json!({"a": {"b": "hello"}});
        assert_eq!(nav(&v, "a.b"), Some(&json!("hello")));
    }

    #[test]
    fn test_nav_leading_dot() {
        let v = json!({"a": {"b": "hello"}});
        assert_eq!(nav(&v, ".a.b"), Some(&json!("hello")));
    }

    #[test]
    fn test_nav_missing() {
        let v = json!({"a": 1});
        assert_eq!(nav(&v, "a.b"), None);
    }

    #[test]
    fn test_nav_array() {
        let v = json!({"items": [10, 20, 30]});
        assert_eq!(nav(&v, "items.1"), Some(&json!(20)));
    }

    #[test]
    fn test_nav_empty_path() {
        let v = json!(42);
        assert_eq!(nav(&v, ""), Some(&json!(42)));
    }
}
