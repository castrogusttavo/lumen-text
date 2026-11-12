package pages

import (
	"net/http"
	"html"
	"io"
)
import ("fmt"
	"example.com/demo/models")

func ghp_virtual_home(w http.ResponseWriter, r *http.Request) {
	_ = w
	_ = r
	user := models.User{Name: "Ada", Admin: true}
    items := []string{"a", "b"}
	io.WriteString(w, html.EscapeString(fmt.Sprint(user.Name)))
	if user.Admin {
	io.WriteString(w, html.EscapeString(fmt.Sprint(user.Greeting())))
	} else if (len(items) > 2) {
	} else {
	}
	for i, it := range items {
	io.WriteString(w, html.EscapeString(fmt.Sprint(fmt.Sprint(i, it))))
	}
	switch user.Name {
	case "Ada":
	default:
	}
}

var _ = http.StatusOK
var _ = fmt.Sprint
var _ = html.EscapeString
var _ = io.EOF
