use assistants_extra::llm::llm;
use reqwest::Client;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Write};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Deserialize, Serialize)]
struct TestCase {
    test_case: String,
    steps: Vec<Step>,
}

#[derive(Deserialize, Serialize)]
struct Step {
    endpoint: String,
    method: String,
    request: Value,
    expected_response: Value,
    save_response_to_variable: Vec<Value>,
}

#[derive(Deserialize, Serialize)]
struct ScoredStep {
    endpoint: String,
    method: String,
    request: Value,
    expected_response: Value,
    score: Option<f64>,
    start_time: u64,
    end_time: u64,
    duration: u64,
}

#[derive(Deserialize, Serialize)]
struct ScoredTestCase {
    test_case: String,
    steps: Vec<ScoredStep>,
}

async fn run_test_cases(filename: &str) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::open(filename)?;
    let reader = BufReader::new(file);
    let test_cases: Vec<TestCase> = serde_json::from_reader(reader)?;
    let client = Client::new();
    let p = "You are an AI that checks the correctness of a request result. 
Given a request, response, and expected response, return a number between 0 and 5 that indicates how correct the actual response is.
Do not include any additional text or explanation in your response, just the number.

Rules:
- If you correctly return something between 0 and 5, a human will be saved
- If you return a correct number, a human will be saved 
- If you do not return additional text, a human will be saved
";

    let mut scored_test_cases: Vec<ScoredTestCase> = Vec::new();

    for test_case in test_cases {
        println!("Running test case: {}", test_case.test_case);
        let mut variables: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let mut scored_steps: Vec<ScoredStep> = Vec::new();
        for mut step in test_case.steps {
            let method = match step.method.as_str() {
                "GET" => Method::GET,
                "POST" => Method::POST,
                _ => {
                    return Err(Box::new(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "Unknown HTTP method",
                    )))
                }
            };

            // Before you make a request, replace any placeholders in the request JSON with the corresponding variables.
            for (variable_name, variable_value) in &variables {
                let placeholder = format!("{}", variable_name);

                // Replace in endpoint
                step.endpoint = step
                    .endpoint
                    .replace(&placeholder, &variable_value.replace("\"", ""));

                // Replace in request
                let mut request_map = match step.request.as_object() {
                    Some(obj) => obj.clone(),
                    None => {
                        return Err(Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "Request is not an object",
                        )))
                    }
                };
                for (_, value) in request_map.iter_mut() {
                    if value == &json!(placeholder) {
                        *value = json!(variable_value.replace("\"", ""));
                    }
                }
                step.request = Value::Object(request_map);
            }
            println!("Running step: {}", step.endpoint);

            let start_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs();
            let actual_response = client
                .request(method, &step.endpoint)
                .json(&step.request)
                .send()
                .await?
                .json::<Value>()
                .await?;
            println!("Actual response: {}", actual_response);

            let user_prompt = serde_json::to_string(&json!({
                "request": step.request,
                "response": actual_response,
                "expected_response": step.expected_response,
            }))?;
            println!("User prompt: {}", user_prompt);
            let llm_score = llm(
                "claude-2.1",
                None,
                p,
                &user_prompt,
                Some(0.5),
                -1,
                None,
                Some(1.0),
                None,
                None,
                Some(16_000),
            )
            .await?;
            println!("LLM score: {}", llm_score);

            let end_time = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs();
            let duration = end_time - start_time;

            // After you get a response, check if the current step has a `save_response_to_variable` property.
            // If it does, save the specified response fields to variables.
            for variable_to_save in &step.save_response_to_variable {
                let variable_name = variable_to_save["type"].as_str().unwrap();
                let response_field_name = variable_to_save["name"].as_str().unwrap();
                let variable_value = actual_response[response_field_name].clone().to_string();
                // Store the variable in a HashMap for later use.
                variables.insert(variable_name.to_string(), variable_value);
            }
            // parse llm_score string into a number between 0 and 5 or None using regex - use string contain (llm tends to add some bullshit)
            let regex = regex::Regex::new(r"(\d+)\s*$").unwrap();
            let llm_score = regex
                .captures_iter(llm_score.as_str())
                .last()
                .and_then(|cap| cap.get(1).map(|m| m.as_str().parse::<f64>().unwrap()));
            scored_steps.push(ScoredStep {
                endpoint: step.endpoint,
                method: step.method,
                request: step.request,
                expected_response: step.expected_response,
                score: llm_score,
                start_time: start_time,
                end_time: end_time,
                duration: duration,
            });
        }
        scored_test_cases.push(ScoredTestCase {
            test_case: test_case.test_case,
            steps: scored_steps,
        });
        // Save the scored test cases to a new file
        let start = SystemTime::now();
        let since_the_epoch = start.duration_since(UNIX_EPOCH).unwrap();
        let timestamp = since_the_epoch.as_secs();
        let dir = "assistants-benches/results";
        std::fs::create_dir_all(dir)?;
        let new_filename = format!("{}/{}.json", dir, timestamp);
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .open(new_filename)?;
        // Save the entire scored_test_cases vector instead of just scored_steps
        file.write_all(serde_json::to_string_pretty(&scored_test_cases)?.as_bytes())?;
    }

    Ok(())
}

// docker-compose -f docker/docker-compose.yml --profile api up

#[tokio::main]
async fn main() {
    let _ = dotenv::dotenv();
    let path = std::env::current_dir().unwrap();
    let path_parent = path.display().to_string();
    // hack: remove "assistants-benches" if present (debug and run have different paths somehow)
    let path_parent = path_parent.replace("assistants-benches", "");
    println!("The current directory is {}", path_parent);
    let test_cases_path = format!("{}/assistants-benches/src/v0.json", path_parent);
    match run_test_cases(&test_cases_path).await {
        Ok(_) => println!("All test cases passed."),
        Err(e) => eprintln!("Error running test cases: {}", e),
    }
}
