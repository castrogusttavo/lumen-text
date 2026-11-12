package pages

import (
	"net/http"
	"fmt"
	"html"
	"io"
)

func ghp_virtual_broken(w http.ResponseWriter, r *http.Request) {
	_ = w
	_ = r
	n := 1
	io.WriteString(w, html.EscapeString(fmt.Sprint(undefinedThing)))
	io.WriteString(w, html.EscapeString(fmt.Sprint(n.Nope)))
}

var _ = http.StatusOK
var _ = fmt.Sprint
var _ = html.EscapeString
var _ = io.EOF
