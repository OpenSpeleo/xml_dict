use pyo3::{
    exceptions::PyValueError,
    prelude::*,
    types::{PyDict, PyList},
};
use quick_xml::events::Event;
use quick_xml::Reader;
use serde_json::{Map, Value};
use std::collections::HashMap;

// XML to Dict implementation

fn parse_xml(xml: &str) -> Result<Value, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack = Vec::new();
    let mut root = None;
    let mut current_value = None;
    let mut current_attrs = HashMap::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_vec();
                let name = String::from_utf8_lossy(&name).to_string();

                // Handle attributes
                let attrs = e
                    .attributes()
                    .filter_map(|a| a.ok())
                    .map(|a| {
                        let key = String::from_utf8_lossy(a.key.as_ref()).to_string();
                        let value = a.unescape_value().unwrap_or_default().to_string();
                        (format!("@{}", key), Value::String(value))
                    })
                    .collect();

                stack.push((name, current_value, current_attrs));
                current_attrs = attrs;
                current_value = Some(Value::Object(Map::new()));
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                if !text.trim().is_empty() {
                    current_value = Some(Value::String(text));
                }
            }
            Ok(Event::End(_)) => {
                let (name, parent_val, parent_attrs) = stack.pop().unwrap();
                let mut obj = match current_value.take() {
                    Some(Value::Object(m)) => m,
                    Some(v) => {
                        let mut m = Map::new();
                        m.insert("#text".to_string(), v);
                        m
                    }
                    None => Map::new(),
                };

                // Merge attributes
                for (k, v) in current_attrs.drain() {
                    obj.insert(k, v);
                }

                current_value = parent_val;
                current_attrs = parent_attrs;

                let new_value = Value::Object(obj);
                if let Some(Value::Object(ref mut parent)) = current_value {
                    // Handle duplicate keys by converting to array
                    if let Some(existing) = parent.get_mut(&name) {
                        if let Value::Array(ref mut arr) = existing {
                            arr.push(new_value);
                        } else {
                            let existing_val = existing.take();
                            parent
                                .insert(name.clone(), Value::Array(vec![existing_val, new_value]));
                        }
                    } else {
                        parent.insert(name, new_value);
                    }
                } else {
                    root = Some(new_value);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                return Err(format!(
                    "Error at position {}: {:?}",
                    reader.buffer_position(),
                    e
                ))
            }
            _ => (),
        }
        buf.clear();
    }

    root.ok_or_else(|| "Empty XML document".to_string())
}

// Dict to XML implementation

fn value_to_xml(
    value: &Value,
    parent_name: &str,
    writer: &mut quick_xml::Writer<Vec<u8>>,
) -> Result<(), String> {
    let mut attributes = Vec::new();
    let mut children = Map::new();
    let mut text = None;

    if let Value::Object(obj) = value {
        for (k, v) in obj {
            if k.starts_with('@') {
                let attr_name = k.trim_start_matches('@');
                attributes.push((
                    attr_name.to_string(),
                    v.as_str().unwrap_or_default().to_string(),
                ));
            } else if k == "#text" {
                text = Some(v.as_str().unwrap_or_default().to_string());
            } else {
                children.insert(k.clone(), v.clone());
            }
        }
    }

    let mut elem = quick_xml::events::BytesStart::new(parent_name);
    for (name, value) in attributes {
        elem.push_attribute((name.as_str(), value.as_str()));
    }

    if children.is_empty() && text.is_none() {
        writer
            .write_event(Event::Empty(elem))
            .map_err(|e| e.to_string())?;
    } else {
        writer
            .write_event(Event::Start(elem))
            .map_err(|e| e.to_string())?;

        if let Some(text_content) = text {
            writer
                .write_event(Event::Text(quick_xml::events::BytesText::new(
                    &text_content,
                )))
                .map_err(|e| e.to_string())?;
        }

        for (name, value) in children {
            match value {
                Value::Array(arr) => {
                    for item in arr {
                        value_to_xml(&item, &name, writer)?;
                    }
                }
                _ => value_to_xml(&value, &name, writer)?,
            }
        }

        writer
            .write_event(Event::End(quick_xml::events::BytesEnd::new(parent_name)))
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

// Python bindings

#[pyfunction]
fn xml_str_to_dict(xml_str: &str) -> PyResult<PyObject> {
    let value = parse_xml(xml_str)
        .map_err(|e| PyValueError::new_err(format!("XML parsing error: {}", e)))?;
    Python::with_gil(|py| value_to_pyobject(&value, py))
}

#[pyfunction]
fn dict_to_xml_str(data: &Bound<'_, PyDict>) -> PyResult<String> {
    let value = pyobject_to_value(data)?;
    let mut writer = quick_xml::Writer::new(Vec::new());
    writer
        .write_event(Event::Decl(quick_xml::events::BytesDecl::new(
            "1.0",
            Some("utf-8"),
            None,
        )))
        .map_err(|e| PyValueError::new_err(format!("XML writing error: {}", e)))?;

    if let Value::Object(root) = value {
        for (name, value) in root {
            value_to_xml(&value, &name, &mut writer)
                .map_err(|e| PyValueError::new_err(format!("XML generation error: {}", e)))?;
        }
    }

    String::from_utf8(writer.into_inner())
        .map_err(|e| PyValueError::new_err(format!("UTF-8 conversion error: {}", e)))
}

// Updated helper functions for Python/Rust type conversion

fn value_to_pyobject(value: &Value, py: Python<'_>) -> PyResult<PyObject> {
    match value {
        Value::Null => Ok(py.None().into_py(py)),
        Value::Bool(b) => Ok(b.into_py(py)),
        Value::Number(n) => Ok(n.as_f64().unwrap().into_py(py)),
        Value::String(s) => Ok(s.into_py(py)),
        Value::Array(arr) => {
            let list = PyList::empty_bound(py);
            for item in arr {
                list.append(value_to_pyobject(item, py)?)?;
            }
            Ok(list.into_py(py))
        }
        Value::Object(obj) => {
            let dict = PyDict::new_bound(py);
            for (k, v) in obj {
                dict.set_item(k, value_to_pyobject(v, py)?)?;
            }
            Ok(dict.into_py(py))
        }
    }
}

fn pyobject_to_value(obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    if let Ok(s) = obj.extract::<String>() {
        Ok(Value::String(s))
    } else if let Ok(n) = obj.extract::<f64>() {
        Ok(Value::from(n))
    } else if let Ok(b) = obj.extract::<bool>() {
        Ok(Value::Bool(b))
    } else if obj.is_none() {
        Ok(Value::Null)
    } else if let Ok(list) = obj.downcast::<PyList>() {
        let mut arr = Vec::new();
        for item in list.iter() {
            arr.push(pyobject_to_value(&item)?);
        }
        Ok(Value::Array(arr))
    } else if let Ok(dict) = obj.downcast::<PyDict>() {
        let mut map = Map::new();
        for (k, v) in dict.iter() {
            let key: String = k.extract()?;
            map.insert(key, pyobject_to_value(&v)?);
        }
        Ok(Value::Object(map))
    } else {
        Err(PyValueError::new_err("Unsupported Python type").into())
    }
}

#[pymodule]
fn xml_dict(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(xml_str_to_dict, m)?)?;
    m.add_function(wrap_pyfunction!(dict_to_xml_str, m)?)?;
    Ok(())
}
