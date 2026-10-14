Make a skill for our HTTP API error conventions, so any agent writing an endpoint or error helper follows them:

- Every error body is JSON: {"error": {"code": ..., "message": ..., "request_id": ...}} and nothing else.
- `code` is one of these, with its HTTP status:
  validation problem → 422 "VALIDATION_FAILED"; missing resource → 404 "NOT_FOUND";
  duplicate resource → 409 "ALREADY_EXISTS"; caller not allowed → 403 "FORBIDDEN";
  anything unexpected → 500 "INTERNAL".
- `message` is the human-readable text passed in, except for INTERNAL, where it is always
  "Internal error" (never leak details).
- `request_id` echoes the caller's X-Request-Id header; if there is none, it is null.
