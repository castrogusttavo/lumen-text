package models

// Address is nested in Team to exercise field completion through a chain.
type Address struct {
	City string
	Zip  string
}

// Team groups users.
type Team struct {
	Name    string
	Members []User
	Lead    *User
	Home    Address
}

// NewTeam builds a team with two members.
func NewTeam(name string) Team {
	ada := User{Name: "Ada", Email: "ada@example.com", Admin: true}
	return Team{
		Name:    name,
		Members: []User{ada, {Name: "Bob"}},
		Lead:    &ada,
		Home:    Address{City: "London", Zip: "N1"},
	}
}

// Size is the number of members.
func (t Team) Size() int { return len(t.Members) }
