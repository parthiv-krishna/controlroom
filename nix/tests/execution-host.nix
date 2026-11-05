# An execution host for the VM end-to-end test (devbox, buildbox): sshd, a user `alice` with a
# bash login shell and git, nothing else. No nono, pi, Node, bundle or model credentials: pi runs
# on the server, and the server bootstraps controlroomd over SSH (as users would), with the nono
# bundled with it, which runs the agent's shells, tools and jobs here.
{ pkgs }:
{
  virtualisation = {
    memorySize = 1536;
    cores = 2;
  };

  services.openssh = {
    enable = true;
    settings = {
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
    };
  };

  users.users.alice = {
    isNormalUser = true;
    shell = pkgs.bashInteractive;
  };

  environment.systemPackages = [ pkgs.git ];
}
