# Without {search: ...}, the search list starts with ".", which jq leaves relative
# to the current directory, not to this module's.
import "nested/inner" as inner;
def here: inner::here;
