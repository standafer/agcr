#![allow(dead_code)]
#![allow(unused_variables)]

use std::{
    error::Error as StdError,
    fs::{self, File},
    io::Write,
    sync::Arc,
};
use serde::{Deserialize, Serialize};
use futures::future::{try_join_all};
use tokio::try_join;
use liquid::{self, model::Value, Object};
use toml;
use reqwest::{Client};

type DbPool = sqlx::SqlitePool;

#[derive(Serialize)]
struct RenderableStudent {
    #[serde(rename = "fullName")]
    full_name: String,
    gpa: f64,
    flags: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
struct GpaResponse {
    #[serde(rename = "GPA_GradeReportingTotal")]
    gpa_grade_reporting_total: f64,
}

#[derive(Deserialize, Debug, Clone)]

struct NameResponse {
    #[serde(rename = "FirstName")]
    first_name: String,
    #[serde(rename = "LastName")]
    last_name: String,
}

#[derive(Deserialize, Debug)]
struct Config {
    timed_reports: Vec<TimedReport>,
}

#[derive(Deserialize, Debug)]
struct TimedReport {
    report_label: String,
    every: String,
    to: Vec<String>,
    template: String,
    flags: Vec<Flag>,
    student_ids: Vec<u32>,
}

#[derive(Deserialize, Debug)]
struct Flag {
    min_gpa: f64,
    max_gpa: f64,
    priority: u8,
    level: String,
}

fn parse_config_toml() -> Config {
    let config_toml = std::fs::read_to_string("config.toml").unwrap();
    let config: Config = toml::from_str(&config_toml).unwrap();
    config
}

fn get_urls_for_id(id: u32) -> (String, String) {
    let info_url = format!(
        "https://demo.aeries.net/aeries/api/v5/schools/994/students/{}?cert=477abe9e7d27439681d62f4e0de1f5e1",
        id
    );
    let gpa_url = format!(
        "https://demo.aeries.net/aeries/api/v5/schools/994/gpas/{}?cert=477abe9e7d27439681d62f4e0de1f5e1",
        id
    );
    (gpa_url, info_url)
}



async fn fetch_students_gpa_and_info(
    ids: Vec<u32>,
) -> Result<Vec<(NameResponse, GpaResponse)>, Box<dyn StdError + Send + Sync>> {
    let client = Arc::new(Client::new());

    let fetch_futures = ids.into_iter().map(|id| {
        let client = Arc::clone(&client);
        async move {
            let (gpa_url, info_url) = get_urls_for_id(id);
            let gpa_future = client.get(&gpa_url).send();
            let info_future = client.get(&info_url).send();
            let (gpa_resp, info_resp) = try_join!(gpa_future, info_future)?;

            let gpa_resp: Vec<GpaResponse> = gpa_resp.json().await?;
            let info_resp: Vec<NameResponse> = info_resp.json().await?;

            match (gpa_resp.into_iter().next(), info_resp.into_iter().next()) {
                (Some(gpa), Some(info)) => Ok((info, gpa)),
                _ => Err(format!("Failed to fetch data for student ID: {}", id).into()),
            }
        }
    });

    let results: Result<Vec<(NameResponse, GpaResponse)>, Box<dyn StdError + Send + Sync>> =
        try_join_all(fetch_futures).await;

    results
}


async fn render_template(template_name: &str, name: String, students: &Vec<RenderableStudent>) -> Result<String, Box<dyn StdError>> {
    let template_path = format!("./templates/{}.liquid", template_name);
    
    let template_str = fs::read_to_string(template_path)?;
    let template = liquid::ParserBuilder::with_stdlib().build()?.parse(&template_str)?;
    
    let mut globals = liquid::model::Object::new();
    globals.insert("name".to_string().into(), Value::scalar(name));
    globals.insert("students".to_string().into(), Value::Array(students.iter().map(|student| {
        let mut student_obj = Object::new();
        student_obj.insert("fullName".to_string().into(), Value::scalar(student.full_name.clone()));
        student_obj.insert("gpa".to_string().into(), Value::scalar(student.gpa));
        student_obj.insert("flags".to_string().into(), Value::Array(student.flags.iter().map(|flag| {
            Value::scalar(flag.clone())
        }).collect()));
        Value::Object(student_obj)
    }).collect()));

    let output = template.render(&globals).unwrap();
    
    Ok(output)
}

#[tokio::main]
async fn main() {
    let config = parse_config_toml();
    let pool = DbPool::connect("sqlite:database.db").await;
    
    for timed_report in &config.timed_reports {
        let mut student_flags: Vec<(NameResponse, GpaResponse, Vec<&Flag>)> = fetch_students_gpa_and_info(timed_report.student_ids.clone())
            .await
            .unwrap()
            .into_iter()
            .map(|(name, gpa)| {
                let flags_met: Vec<&Flag> = timed_report.flags.iter().filter(|flag| {
                    gpa.gpa_grade_reporting_total >= flag.min_gpa
                    && gpa.gpa_grade_reporting_total < flag.max_gpa
                }).collect();
                (name, gpa, flags_met)
            })
            .collect();
        
        // Sort the students by the highest flag priority they meet
        student_flags.sort_by(|a, b| {
            let a_priority = a.2.iter().map(|flag| flag.priority).max().unwrap_or(0);
            let b_priority = b.2.iter().map(|flag| flag.priority).max().unwrap_or(0);
            b_priority.cmp(&a_priority)
        });
        
        for (name, _, flags) in &student_flags {
            println!("Student: {} {}", name.first_name, name.last_name);
            if flags.is_empty() {
                println!("\tNo flags met");
            } else {
                println!("\tFlags met: {:?}", flags.to_vec().iter().map(|flag| flag.level.clone()).collect::<Vec<String>>())
            }
        }

        let mut renderable_students: Vec<RenderableStudent> = vec![];
        for (name, gpa, flags) in &student_flags {
            let student_flags: Vec<String> = flags.iter().map(|flag| flag.level.clone()).collect();
            renderable_students.push(
                RenderableStudent {
                    full_name: format!("{} {}", name.first_name, name.last_name),
                    gpa: gpa.gpa_grade_reporting_total,
                    flags: student_flags,
                }
            )
        }

        let rendered_template = render_template(&timed_report.template, "Mr. Smith".to_string(), &renderable_students).await.unwrap();
        // export into html file
        let mut file = File::create(format!("{}.html", timed_report.template)).unwrap();
        file.write_all(rendered_template.as_bytes()).unwrap();
    }
}
