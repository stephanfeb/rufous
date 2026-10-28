---
name: security_review
description: Security checklist: injection / path traversal / sensitive data / permissions
---
# Security check
1. Input validation: shell command concatenation -> command injection; SQL string concatenation -> SQL injection; uncontrolled path joining -> path traversal
2. Sensitive data: hard-coded passwords / API keys / secrets -> must be removed or moved to environment variables
3. Permissions: check whether file paths can escape (..) and whether writes can land anywhere
4. Fixes: prefer parameterised queries, allow-list validation, subprocess with an argument list (no shell=True string building)
5. After fixing, run the tests to confirm behaviour is unchanged
