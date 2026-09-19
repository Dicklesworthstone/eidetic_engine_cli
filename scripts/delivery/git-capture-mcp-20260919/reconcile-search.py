"""Assert the real typed search identity rather than an absent generic id."""
from pathlib import Path
path = Path('tests/mcp_capture_git.rs')
source = path.read_text()
old = 'row["id"].as_str() == Some(id)'
new = '(row["memoryId"].as_str() == Some(id) && row["docId"].as_str() == Some(id))'
if old in source:
    assert source.count(old) == 1
    path.write_text(source.replace(old, new, 1))
else:
    assert 'row["memoryId"].as_str() == Some(id)' in source
    assert 'row["docId"].as_str() == Some(id)' in source
