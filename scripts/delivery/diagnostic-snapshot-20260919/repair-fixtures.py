"""Repair only the unformatted native-rule test fixtures using the real DB API."""
from pathlib import Path
import sys


def repair(s):
    s = s.replace('    use sqlmodel_core::Value;\n', '')
    old = '''        for update in [
            "UPDATE procedural_rules SET tombstoned_at = '2026-09-01T00:00:00Z' WHERE id = ?1",
            "UPDATE procedural_rules SET tombstoned_at = NULL, maturity = 'superseded' WHERE id = ?1",
            "UPDATE procedural_rules SET maturity = 'validated', superseded_by = ?2 WHERE id = ?1",
        ] {
            let params = if update.contains("?2") {
                vec![Value::Text(RULE.to_owned()), Value::Text(SECOND.to_owned())]
            } else { vec![Value::Text(RULE.to_owned())] };
            db.execute_with_params(update, &params).map_err(|e| e.to_string())?;'''
    new = '''        for update in [
            format!("UPDATE procedural_rules SET tombstoned_at = '2026-09-01T00:00:00Z' WHERE id = '{RULE}'"),
            format!("UPDATE procedural_rules SET tombstoned_at = NULL, maturity = 'superseded' WHERE id = '{RULE}'"),
            format!("UPDATE procedural_rules SET maturity = 'validated', superseded_by = '{SECOND}' WHERE id = '{RULE}'"),
        ] {
            db.execute_raw(&update).map_err(|e| e.to_string())?;'''
    assert s.count(old) == 1
    s = s.replace(old, new)
    old = '''        db.execute_with_params("INSERT INTO rule_tags (rule_id, tag) VALUES (?1, 'changed')",
            &[Value::Text(RULE.to_owned())]).map_err(|e| e.to_string())?;'''
    new = '''        db.execute_raw(&format!("INSERT INTO rule_tags (rule_id, tag) VALUES ('{RULE}', 'changed')"))
            .map_err(|e| e.to_string())?;'''
    assert s.count(old) == 1
    s = s.replace(old, new)
    old = '''        writer.execute_with_params("UPDATE procedural_rules SET tombstoned_at = '2026-09-01T00:00:00Z' WHERE id = ?1",
            &[Value::Text(RULE.to_owned())]).map_err(|e| e.to_string())?;'''
    new = '''        writer.execute_raw(&format!("UPDATE procedural_rules SET tombstoned_at = '2026-09-01T00:00:00Z' WHERE id = '{RULE}'"))
            .map_err(|e| e.to_string())?;'''
    s = s.replace(old, new)
    old = '''        db.execute_with_params("UPDATE procedural_rules SET content = ?2 WHERE id = ?1",
            &[Value::Text(RULE.to_owned()), Value::Text(body.clone())]).map_err(|e| e.to_string())?;'''
    new = '''        db.execute_raw(&format!("UPDATE procedural_rules SET content = '{}' WHERE id = '{RULE}'", body.replace('\\'', "''")))
            .map_err(|e| e.to_string())?;'''
    s = s.replace(old, new)
    assert 'execute_with_params' not in s
    assert 'Value::Text' not in s
    return s


if __name__ == '__main__':
    path = Path(sys.argv[1])
    path.write_text(repair(path.read_text()))
