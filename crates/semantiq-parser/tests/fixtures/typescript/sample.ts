import { Component } from "react";

interface User {
    name: string;
}

type UserId = string;

enum Status {
    Active,
    Inactive,
}

class UserService {
    private users: Map<UserId, User> = new Map();
    addUser(id: UserId, user: User): void {
        this.users.set(id, user);
    }
}

export const fadeIn = (n: number) => n + 1;
export const config = { debug: true };

export function greet(name: string): string {
    return `Hello, ${name}!`;
}
