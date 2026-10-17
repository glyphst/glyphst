//! Reading and writing plain KDL values.

use kdl::{KdlDocument, KdlNode, KdlValue};

pub fn value_to_f32(value: &KdlValue) -> Option<f32> {
    match value {
        KdlValue::Float(value) if value.is_finite() => Some(*value as f32),
        KdlValue::Integer(value) => Some(*value as f32),
        _ => None,
    }
}

pub fn push_value_node(document: &mut KdlDocument, name: &str, value: impl Into<KdlValue>) {
    let mut node = KdlNode::new(name);
    node.push(value);
    document.nodes_mut().push(node);
}
