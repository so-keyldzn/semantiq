package main

import "fmt"

type User struct {
	Name string
}

type Greeter interface {
	Greet() string
}

func (u *User) Greet() string {
	return fmt.Sprintf("Hi %s", u.Name)
}

func main() {
	fmt.Println("hi")
}

const Pi = 3.14
var counter = 0
