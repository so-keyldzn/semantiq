class User
  def initialize(name)
    @name = name
  end

  def greet
    @name
  end
end

module Utils
  def self.format(name)
    name.strip
  end
end

VERSION = "1.0"
