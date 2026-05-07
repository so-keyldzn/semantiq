using System;

namespace MyApp.Services {
    public class UserService {
        private List<User> users;

        public void AddUser(User user) {
            users.Add(user);
        }
    }

    public struct User {
        public int Id;
    }

    public interface IGreeter {
        string Greet();
    }

    public enum Status {
        Active,
        Inactive
    }
}
