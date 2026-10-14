Create errors.py with a function error_response(kind, message, headers) for our HTTP API, following our
API error conventions. kind is one of "validation", "not_found", "duplicate", "forbidden", "unexpected";
headers is a dict of request headers. It returns (status_code, body_dict).
