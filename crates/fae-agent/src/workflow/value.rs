use std::collections::HashMap;

use serde_json::{Map, Number, Value};

use super::{WorkflowCompare, WorkflowCondition};

#[derive(Debug)]
pub struct WorkflowValues<'a> {
    pub input: &'a Value,
    pub outputs: &'a HashMap<String, Value>,
    pub loops: &'a HashMap<String, usize>,
    pub last_output: Option<&'a Value>,
}

impl WorkflowValues<'_> {
    pub fn resolve(&self, template: &Value) -> anyhow::Result<Value> {
        match template {
            Value::String(value) => self.resolve_string(value),
            Value::Array(values) => values
                .iter()
                .map(|value| self.resolve(value))
                .collect::<anyhow::Result<Vec<_>>>()
                .map(Value::Array),
            Value::Object(values) => values
                .iter()
                .map(|(key, value)| Ok((key.clone(), self.resolve(value)?)))
                .collect::<anyhow::Result<Map<_, _>>>()
                .map(Value::Object),
            value => Ok(value.clone()),
        }
    }

    pub fn evaluate(&self, condition: &WorkflowCondition) -> anyhow::Result<bool> {
        match condition {
            WorkflowCondition::Truthy { value } => Ok(is_truthy(&self.resolve(value)?)),
            WorkflowCondition::Exists { value } => Ok(self.resolve(value).is_ok()),
            WorkflowCondition::Compare { left, op, right } => {
                let left = self.resolve(left)?;
                let right = self.resolve(right)?;
                compare(&left, *op, &right)
            }
        }
    }

    fn resolve_string(&self, template: &str) -> anyhow::Result<Value> {
        if let Some(reference) = exact_reference(template) {
            return self.resolve_reference(reference);
        }

        let mut rendered = String::with_capacity(template.len());
        let mut remaining = template;
        while let Some(start) = remaining.find("{$") {
            rendered.push_str(&remaining[..start]);
            let reference_start = start + 2;
            let Some(relative_end) = remaining[reference_start..].find('}') else {
                anyhow::bail!("unterminated workflow value reference in `{template}`");
            };
            let end = reference_start + relative_end;
            let value = self.resolve_reference(&remaining[reference_start..end])?;
            rendered.push_str(&display_value(&value));
            remaining = &remaining[end + 1..];
        }
        rendered.push_str(remaining);
        Ok(Value::String(rendered))
    }

    fn resolve_reference(&self, reference: &str) -> anyhow::Result<Value> {
        let mut segments = reference.split('.');
        let root = segments
            .next()
            .filter(|root| !root.is_empty())
            .ok_or_else(|| anyhow::anyhow!("workflow value reference cannot be empty"))?;

        let mut value =
            match root {
                "input" => self.input.clone(),
                "last" => self
                    .last_output
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("workflow has no previous node output"))?,
                "loop" => {
                    let loop_id = segments.next().ok_or_else(|| {
                        anyhow::anyhow!("loop reference must contain a loop node id")
                    })?;
                    let iteration = self
                        .loops
                        .get(loop_id)
                        .ok_or_else(|| anyhow::anyhow!("loop `{loop_id}` has not started"))?;
                    let mut object = Map::new();
                    object.insert(
                        "iteration".to_string(),
                        Value::Number(Number::from(*iteration)),
                    );
                    Value::Object(object)
                }
                node_id => self.outputs.get(node_id).cloned().ok_or_else(|| {
                    anyhow::anyhow!("node `{node_id}` has not produced an output")
                })?,
            };

        for segment in segments {
            value = match value {
                Value::Object(mut object) => object.remove(segment).ok_or_else(|| {
                    anyhow::anyhow!(
                        "field `{segment}` does not exist in reference `{{$reference}}`"
                    )
                })?,
                Value::Array(array) => {
                    let index = segment.parse::<usize>().map_err(|_| {
                        anyhow::anyhow!(
                            "`{segment}` is not an array index in reference `{{$reference}}`"
                        )
                    })?;
                    array.get(index).cloned().ok_or_else(|| {
                        anyhow::anyhow!(
                            "array index `{index}` is out of bounds in reference `{{$reference}}`"
                        )
                    })?
                }
                _ => anyhow::bail!(
                    "cannot select `{segment}` from a scalar in reference `{{$reference}}`"
                ),
            };
        }
        Ok(value)
    }
}

fn exact_reference(value: &str) -> Option<&str> {
    value
        .strip_prefix("{$")
        .and_then(|value| value.strip_suffix('}'))
        .filter(|value| !value.contains('{') && !value.contains('}'))
}

fn display_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        value => value.to_string(),
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn compare(left: &Value, op: WorkflowCompare, right: &Value) -> anyhow::Result<bool> {
    match op {
        WorkflowCompare::Eq => Ok(left == right),
        WorkflowCompare::Ne => Ok(left != right),
        WorkflowCompare::Gt | WorkflowCompare::Ge | WorkflowCompare::Lt | WorkflowCompare::Le => {
            let order = match (left, right) {
                (Value::Number(left), Value::Number(right)) => left
                    .as_f64()
                    .and_then(|left| right.as_f64().and_then(|right| left.partial_cmp(&right))),
                (Value::String(left), Value::String(right)) => Some(left.cmp(right)),
                _ => None,
            }
            .ok_or_else(|| {
                anyhow::anyhow!("ordered workflow comparison requires two numbers or two strings")
            })?;
            Ok(match op {
                WorkflowCompare::Gt => order.is_gt(),
                WorkflowCompare::Ge => order.is_ge(),
                WorkflowCompare::Lt => order.is_lt(),
                WorkflowCompare::Le => order.is_le(),
                WorkflowCompare::Eq | WorkflowCompare::Ne => unreachable!(),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_typed_and_embedded_references() {
        let outputs = HashMap::from([("A".to_string(), json!({"output_field": {"count": 3}}))]);
        let loops = HashMap::new();
        let values = WorkflowValues {
            input: &json!({"name": "Ada"}),
            outputs: &outputs,
            loops: &loops,
            last_output: outputs.get("A"),
        };

        assert_eq!(
            values.resolve(&json!("{$A.output_field.count}")).unwrap(),
            json!(3)
        );
        assert_eq!(
            values.resolve(&json!("hello {$input.name}")).unwrap(),
            json!("hello Ada")
        );
    }
}
