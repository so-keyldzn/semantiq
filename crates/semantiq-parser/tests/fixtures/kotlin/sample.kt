package com.example
import kotlin.io.println

interface Greeter {
    fun greet(): String
}

enum class Status {
    ACTIVE,
    INACTIVE
}

class User(val name: String) {
    fun greet(): String = "Hello $name"
}

object Singleton {
    fun helper() = 1
}

fun main() {
    println("hi")
}
