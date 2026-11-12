package models

// User is a demo model used by the pages.
type User struct {
	Name  string
	Email string
	Admin bool
}

// Greeting returns a greeting for the user.
func (u User) Greeting() string { return "Hello, " + u.Name }
