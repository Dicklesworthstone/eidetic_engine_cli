//! A real CLI answer must not label compatible numeric restrictions as a dispute.
use super::*;

#[test]
fn public_compatible_numeric_restriction_is_not_a_conflict() -> Result<(), String> {
    let (_root, workspace, database) = super::super::super::build_empty_workspace()?;
    let compatible = "The production database service port is not 6432.";
    let ids = populate(&workspace, &database, compatible, None)?;
    let (exit, response) = run(&workspace, None)?;
    assert_eq!(exit, Some(0));
    let data = &response["data"];
    assert_eq!(data["abstained"], false);
    assert!(data["sides"].is_null());
    assert_eq!(data["confidenceComponents"]["contradictionPenalty"], 0.0);
    let citations = data["citations"]
        .as_array()
        .ok_or("missing compatible citations")?;
    assert_eq!(citations.len(), 2);
    for (id, text) in ids.iter().zip([FIRST, compatible]) {
        let citation = citations
            .iter()
            .find(|row| row["memoryId"].as_str() == Some(id.as_str()))
            .ok_or("compatible source was discarded")?;
        assert_eq!(citation["text"], text);
        let start = citation["span"]["byteStart"]
            .as_u64()
            .ok_or("missing byte start")? as usize;
        let end = citation["span"]["byteEnd"]
            .as_u64()
            .ok_or("missing byte end")? as usize;
        assert_eq!(text.get(start..end), citation["text"].as_str());
    }
    assert!(
        !response["degraded"]
            .as_array()
            .ok_or("missing degradations")?
            .iter()
            .any(|row| row["code"] == "ask_conflicting_evidence")
    );
    Ok(())
}
