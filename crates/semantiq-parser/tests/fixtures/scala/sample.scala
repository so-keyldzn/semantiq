import scala.collection.mutable

class Calculator(val initial: Int) {
  def add(n: Int): Int = initial + n
}

object Helpers {
  def util(): Int = 1
  val PI: Double = 3.14
}

trait Greeter {
  def greet(): String
}
