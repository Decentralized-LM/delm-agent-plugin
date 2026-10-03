use crate::protocol::Event;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(super) struct Pending {
    pub native_id: Value,
    pub worker: usize,
    pub turn: String,
    pub items: Value,
}

#[derive(Default)]
pub(super) struct Questions(HashMap<String, Pending>);

pub(crate) fn validate_answers(
    items: &Value,
    answers: &BTreeMap<String, Vec<String>>,
) -> Result<()> {
    let items = items.as_array().context("Question items are missing")?;
    let ids = items
        .iter()
        .map(|item| item["id"].as_str().context("Question ID is missing"))
        .collect::<Result<HashSet<_>>>()?;
    ensure!(
        !ids.is_empty() && ids.len() == items.len(),
        "Question IDs must be nonempty and unique"
    );
    ensure!(
        answers.len() == ids.len() && answers.keys().all(|id| ids.contains(id.as_str())),
        "Answer exactly the question IDs in this request"
    );
    ensure!(
        answers
            .values()
            .all(|values| !values.is_empty() && values.iter().all(|v| !v.trim().is_empty()))
            && serde_json::to_vec(answers)?.len() <= 64 * 1024,
        "Answers are empty or exceed 64 KiB"
    );
    Ok(())
}

impl Questions {
    pub fn insert(
        &mut self,
        native_id: Value,
        worker: usize,
        turn: String,
        items: Value,
    ) -> Result<Event> {
        let list = items.as_array().context("Question list is missing")?;
        ensure!(
            !list.is_empty() && list.len() <= 3,
            "Expected one to three worker questions"
        );
        let ids = list
            .iter()
            .map(|item| {
                item["id"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .context("Question ID is missing")
            })
            .collect::<Result<HashSet<_>>>()?;
        ensure!(ids.len() == list.len(), "Question IDs must be unique");
        ensure!(
            list.iter().all(|item| item["question"]
                .as_str()
                .is_some_and(|text| !text.is_empty())),
            "Question text is missing"
        );
        let id = uuid::Uuid::new_v4().to_string();
        let mut event = Event::new(
            "question",
            format!("Worker {} needs your input.", worker + 1),
        );
        event.id = Some(id.clone());
        event.details = Some(json!({"worker":worker+1,"questions":items}));
        self.0.insert(
            id,
            Pending {
                native_id,
                worker,
                turn,
                items,
            },
        );
        Ok(event)
    }

    pub fn take(&mut self, id: &str, answers: &BTreeMap<String, Vec<String>>) -> Result<Pending> {
        let pending = self
            .0
            .get(id)
            .context("This worker question is no longer pending")?;
        validate_answers(&pending.items, answers)?;
        Ok(self.0.remove(id).expect("validated pending question"))
    }

    pub fn resolve(&mut self, native_id: &Value) -> Option<Event> {
        let id = self
            .0
            .iter()
            .find(|(_, q)| &q.native_id == native_id)
            .map(|(id, _)| id.clone())?;
        self.0.remove(&id);
        Some(resolved(&id))
    }

    pub fn retire_turn(&mut self, worker: usize, turn: &str) -> Vec<Event> {
        let ids = self
            .0
            .iter()
            .filter(|(_, q)| q.worker == worker && q.turn == turn)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        ids.into_iter()
            .map(|id| {
                self.0.remove(&id);
                resolved(&id)
            })
            .collect()
    }
}

pub(super) fn resolved(id: &str) -> Event {
    let mut event = Event::new(
        "question_resolved",
        "The worker question is no longer pending.",
    );
    event.id = Some(id.to_owned());
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_questions_keep_choices_and_require_correlated_answers() {
        let mut questions = Questions::default();
        let items = json!([{"id":"format","question":"Which output?","options":[{"label":"SVG","description":"Editable vector"}]}]);
        let one = questions
            .insert(json!(1), 0, "turn-1".into(), items.clone())
            .unwrap();
        let two = questions
            .insert(json!(2), 1, "turn-2".into(), items.clone())
            .unwrap();
        assert_eq!(one.details.as_ref().unwrap()["questions"], items);
        assert!(
            questions
                .take(
                    one.id.as_ref().unwrap(),
                    &BTreeMap::from([("other".into(), vec!["SVG".into()])])
                )
                .is_err()
        );
        let answers = BTreeMap::from([("format".into(), vec!["SVG".into()])]);
        assert_eq!(
            questions
                .take(one.id.as_ref().unwrap(), &answers)
                .unwrap()
                .native_id,
            json!(1)
        );
        assert!(questions.take(one.id.as_ref().unwrap(), &answers).is_err());
        assert!(questions.take(two.id.as_ref().unwrap(), &answers).is_ok());
    }
}
