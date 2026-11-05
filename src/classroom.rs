use crate::web::Web;
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use chrono::{DateTime, Local, TimeZone};
use serde_json::{Value, json};

const COURSES: &str = r#"[[100,null,1,0],[1,1,null,null,1,1,null,1,[null,1,null,1,null,1,[1,1,1,1,1,1],null,[1],1,1,1,1,[1,1],[1,1]],1,1,null,null,1,null,null,null,null,null,null,[1,1,1,1,1,1,1],1,null,null,1,null,null,null,[1,1],1,1,null,null,1,1,1,1,null,1,1,[null,null,null,null,1,1,null,null,1,null,null,null,null,null,null,null,null,1,1,1,1,1,1],null,[1,1,1],[1,1,null,1],1,1,1,[[[1,1,1],1],[[[1,1]]],1],null,1,[1,1],null,null,1,null,[null,null,1],1],[[null,[[1]]],null,null,[1,2,3],null,null,[1,2]]]"#;
const COURSEWORK: &str = r#"[[100,null,1,0],[[[1,1,1,1,1,null,null,[1,1,1,null,1,1,1],1,1,1,1,1,1,null,null,null,null,1,null,null,null,1,[1],1,[null,null,1,1,1,null,1]],[1,1,1,1,1,1,[1],1,null,[1,1],1,1,null,1,[[1,1,[],[null,1]],1,1],null,null,null,1],[null,1],null,[1,1]],[[1,1,1,1,1,null,null,[1,1,1,null,1,1,1],1,1,1,1,1,1,null,null,null,null,1,null,null,null,1,[1],1,[null,null,1,1,1,null,1]]],[[1,1,1,1,1,null,null,[1,1,1,null,1,1,1],1,1,1,1,1,1,null,null,null,null,1,null,null,null,1,[1],1,[null,null,1,1,1,null,1]],[1,1,1,1,1,1,[1],1,null,[1,1],1,1,null,1,[[1,1,[],[null,1]],1,1],null,null,null,1],[1]],null,null,[[1,1,1,1,1,null,null,[1,1,1,null,1,1,1],1,1,1,1,1,1,null,null,null,null,1,null,null,null,1,[1],1,[null,null,1,1,1,null,1]]]],[[null,[[0]],[2,5],[2],null,null,null,null,null,null,null,null,null,null,[1,2]]]]"#;
const MATERIALS: &str = r#"[[null,null,1,0],[[0,[0]]],[[null,1,1,1,1,1,null,1,1,1,1,1,1,null,[[1,1,1,1,1],1,1,1,1,1,1],[[1,1,1,1,1],1,1,1,[1,1]],1,null,1,[[1,1,1,1,1],1,1],1,[[1,1,1,1,1],1]]]]"#;
const SUB_MASK: &str = r#"[null,1,null,1,1,null,1,1,1,null,null,null,1,null,[1],null,null,null,1,1,1,null,null,null,[[1,1]],[[1,1]],[1,[[1,1,[],[null,1,1]],1,1,1]],null,null,null,null,1]"#;

const STREAM_KINDS: &[i64] = &[2, 3, 5, 7];
const TODO_FILTER: &[i64] = &[1, 2];
const DONE_FILTER: &[i64] = &[3, 4, 8, 10, 5, 7, 9, 6, 11];

#[derive(Debug, Clone)]
pub struct Course {
    pub id: String,
    pub name: String,
    pub section: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Task {
    pub id: String,
    pub course_id: String,
    pub title: String,
    pub description: Option<String>,
    pub due: Option<DateTime<Local>>,
    pub max_points: Option<f64>,
    pub created: Option<DateTime<Local>>,
    pub done: bool,
}

impl Task {
    pub fn link(&self) -> String {
        task_link(&self.course_id, &self.id)
    }
}

pub fn task_link(course_id: &str, task_id: &str) -> String {
    format!(
        "https://classroom.google.com/c/{}/a/{}/details",
        STANDARD_NO_PAD.encode(course_id),
        STANDARD_NO_PAD.encode(task_id)
    )
}

#[derive(Debug, Clone, PartialEq)]
pub enum MaterialKind {
    Drive { id: String, mime: Option<String> },
    YouTube,
    Form,
    Link,
}

#[derive(Debug, Clone)]
pub struct Material {
    pub title: String,
    pub kind: MaterialKind,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    New,
    TurnedIn,
    Returned,
    Created,
    Reclaimed,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::New | State::Created => "assigned",
            State::TurnedIn => "turned in",
            State::Returned => "returned",
            State::Reclaimed => "unsubmitted",
        }
    }

    pub fn is_done(self) -> bool {
        matches!(self, State::TurnedIn | State::Returned)
    }
}

#[derive(Debug, Clone)]
pub struct Submission {
    pub state: State,
    pub grade: Option<f64>,
    pub attachments: Vec<Material>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PostKind {
    Assignment,
    Announcement,
    Question,
    Material,
}

impl PostKind {
    pub fn label(self) -> &'static str {
        match self {
            PostKind::Assignment => "assignment",
            PostKind::Announcement => "announcement",
            PostKind::Question => "question",
            PostKind::Material => "material",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Post {
    pub id: String,
    pub course_id: String,
    pub kind: PostKind,
    pub title: String,
    pub text: Option<String>,
    pub created: Option<DateTime<Local>>,
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn num(id: &str) -> Result<i64> {
    id.parse()
        .with_context(|| format!("Invalid Classroom ID: {id}"))
}

fn template(t: &str) -> Value {
    serde_json::from_str(t).expect("invalid request template")
}

fn set(v: &mut Value, ptr: &str, value: Value) {
    *v.pointer_mut(ptr).expect("invalid template path") = value;
}

fn parse_material(att: &Value) -> Option<Material> {
    let title = text(&att[0]).unwrap_or_default();
    let target = text(&att[2]);
    let url = text(&att[6]).or_else(|| target.clone());
    let kind = match att[3].as_i64()? {
        2 => MaterialKind::Drive {
            id: target?,
            mime: text(&att[4]),
        },
        3 => MaterialKind::YouTube,
        7 => MaterialKind::Form,
        _ => MaterialKind::Link,
    };
    Some(Material { title, kind, url })
}

fn parse_submission(sub: &Value) -> Submission {
    let state = match sub[5].as_i64() {
        Some(2) => State::TurnedIn,
        Some(3) => State::Returned,
        Some(4) => State::Created,
        Some(5) => State::Reclaimed,
        _ => State::New,
    };
    Submission {
        state,
        grade: sub[8].as_f64(),
        attachments: sub[4]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_material)
            .collect(),
    }
}

fn parse_task(w: &Value, done: bool) -> Option<Task> {
    let item = &w[1][0];
    let details = &w[1][1];
    let description = text(&item[27][0][6][1]).filter(|s| !s.trim().is_empty());
    Some(Task {
        id: text(&item[0][0])?,
        course_id: text(&item[0][1][0])?,
        title: text(&item[5]).unwrap_or_default(),
        description,
        due: details[0]
            .as_i64()
            .and_then(|ms| Local.timestamp_millis_opt(ms).single()),
        max_points: details[3].as_f64(),
        created: item[1]
            .as_i64()
            .and_then(|ms| Local.timestamp_millis_opt(ms).single()),
        done,
    })
}

fn parse_post(w: &Value) -> Option<Post> {
    let kind = match w[0].as_i64()? {
        2 => PostKind::Assignment,
        3 => PostKind::Announcement,
        5 => PostKind::Question,
        7 => PostKind::Material,
        _ => return None,
    };
    let item = w
        .as_array()?
        .iter()
        .skip(1)
        .find(|slot| slot.is_array())
        .map(|slot| &slot[0])?;
    Some(Post {
        id: text(&item[0][0])?,
        course_id: text(&item[0][1][0])?,
        kind,
        title: text(&item[5]).unwrap_or_default(),
        text: text(&item[27][0][6][1]).filter(|s| !s.trim().is_empty()),
        created: item[1]
            .as_i64()
            .and_then(|ms| Local.timestamp_millis_opt(ms).single()),
    })
}

impl Web {
    fn sub_ref(&self, course_id: &str, task_id: &str) -> Result<Value> {
        Ok(json!([
            num(&self.user_id)?,
            [num(task_id)?, [num(course_id)?]]
        ]))
    }

    pub fn courses(&self) -> Result<Vec<Course>> {
        let r = self.rpc("gXtzob", &template(COURSES), "/")?;
        Ok(r[2]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some(Course {
                    id: text(&c[0][0])?,
                    name: text(&c[5])?,
                    section: text(&c[8]),
                })
            })
            .collect())
    }

    pub fn tasks(&self, course_id: &str, done: bool) -> Result<Vec<Task>> {
        let mut args = template(COURSEWORK);
        set(&mut args, "/2/0/1", json!([[num(course_id)?]]));
        let filter = if done { DONE_FILTER } else { TODO_FILTER };
        set(&mut args, "/2/0/14", json!(filter));
        let r = self.rpc("pONvgf", &args, "/a/not-turned-in/all")?;
        Ok(r[2]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|w| parse_task(w, done))
            .collect())
    }

    pub fn task(&self, course_id: &str, task_id: &str) -> Result<Task> {
        for done in [false, true] {
            if let Some(t) = self
                .tasks(course_id, done)?
                .into_iter()
                .find(|t| t.id == task_id)
            {
                return Ok(t);
            }
        }
        bail!("Assignment {task_id} not found in course {course_id}")
    }

    pub fn course(&self, course_id: &str) -> Result<Course> {
        self.courses()?
            .into_iter()
            .find(|c| c.id == course_id)
            .with_context(|| format!("You are not enrolled in course {course_id}"))
    }

    pub fn materials(&self, course_id: &str, task_id: &str) -> Result<Vec<Material>> {
        let mut args = template(MATERIALS);
        set(&mut args, "/1", json!([[num(task_id)?, [num(course_id)?]]]));
        let r = self.rpc("t51ITc", &args, "/")?;
        Ok(r[1]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| parse_material(&m[0]))
            .collect())
    }

    pub fn stream(&self, course_id: &str) -> Result<Vec<Post>> {
        let mut args = template(COURSEWORK);
        set(&mut args, "/2/0/1", json!([[num(course_id)?]]));
        set(&mut args, "/2/0/2", json!(STREAM_KINDS));
        set(&mut args, "/2/0/14", Value::Null);
        let r = self.rpc("pONvgf", &args, "/")?;
        Ok(r[2]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_post)
            .collect())
    }

    pub fn submissions(
        &self,
        course_id: &str,
        task_ids: &[String],
    ) -> Result<Vec<(String, Submission)>> {
        if task_ids.is_empty() {
            return Ok(vec![]);
        }
        let refs = task_ids
            .iter()
            .map(|t| self.sub_ref(course_id, t))
            .collect::<Result<Vec<_>>>()?;
        let args = json!([
            [null, null, 2, 0],
            template(SUB_MASK),
            [null, null, refs],
            1
        ]);
        let r = self.rpc("Zj93ge", &args, "/")?;
        Ok(r[2]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|sub| Some((text(&sub[0][1][0])?, parse_submission(sub))))
            .collect())
    }

    pub fn submission(&self, course_id: &str, task_id: &str) -> Result<Submission> {
        let args = json!([
            [null, null, 2, 0],
            template(SUB_MASK),
            [null, null, [self.sub_ref(course_id, task_id)?]],
            1
        ]);
        let r = self.rpc("Zj93ge", &args, "/")?;
        let sub = &r[2][0];
        if sub.is_null() {
            bail!("Your submission for this assignment was not found");
        }
        Ok(parse_submission(sub))
    }

    fn modify(
        &self,
        course_id: &str,
        task_id: &str,
        change: Value,
        mask: Value,
    ) -> Result<Submission> {
        let r = self.sub_ref(course_id, task_id)?;
        let mut patch = change;
        patch[0] = r.clone();
        let args = json!([[3], [[r, patch, mask]], template(SUB_MASK)]);
        let resp = self.rpc(
            "CVt8yf",
            &args,
            &format!(
                "/c/{}/a/{}/details",
                STANDARD_NO_PAD.encode(course_id),
                STANDARD_NO_PAD.encode(task_id)
            ),
        )?;
        Ok(parse_submission(&resp[1][0]))
    }

    pub fn attach_drive_file(
        &self,
        course_id: &str,
        task_id: &str,
        file_id: &str,
        mime: &str,
    ) -> Result<Submission> {
        let temp_id = -((rand::random::<u64>() % 90_000_000_000 + 10_000_000_000) as i64);
        let attachment = json!([
            null,
            null,
            file_id,
            2,
            mime,
            null,
            null,
            1,
            null,
            null,
            null,
            null,
            null,
            null,
            [null, 2],
            null,
            null,
            temp_id,
            null,
            null,
            1
        ]);
        self.modify(
            course_id,
            task_id,
            json!([null, null, null, null, [attachment]]),
            json!([null, 1]),
        )
    }

    pub fn turn_in(&self, course_id: &str, task_id: &str) -> Result<Submission> {
        let mut change = vec![Value::Null; 33];
        change[5] = json!(2);
        change[32] = json!(1);
        let mut mask = vec![Value::Null; 13];
        mask[0] = json!(1);
        mask[12] = json!(1);
        self.modify(course_id, task_id, Value::Array(change), Value::Array(mask))
    }

    pub fn reclaim(&self, course_id: &str, task_id: &str) -> Result<Submission> {
        let mut change = vec![Value::Null; 6];
        change[5] = json!(5);
        self.modify(course_id, task_id, Value::Array(change), json!([1]))
    }
}
