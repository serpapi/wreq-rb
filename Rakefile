# frozen_string_literal: true

require "bundler/gem_tasks"
require "rake/extensiontask"
require "rb_sys"
require "rb_sys/extensiontask"
require "minitest/test_task"

GEMSPEC = Gem::Specification.load("wreq-rb.gemspec")

RbSys::ExtensionTask.new("wreq_rb", GEMSPEC) do |ext|
  ext.lib_dir = "lib/wreq_rb"
  ext.cross_compile = true
  ext.cross_platform = RbSys::ToolchainInfo.supported_ruby_platforms
end

Minitest::TestTask.create(:test) do |t|
  t.libs << "test"
  t.libs << "lib"
  t.test_globs = ["test/**/*_test.rb"]
end

# Reset vendored submodules to clean state on rake clean
task :reset_submodules do
  puts "Resetting vendored submodules..."
  sh "git submodule foreach git reset --hard"
  sh "git submodule foreach git clean -fd"
end
task clean: :reset_submodules

task default: %i[compile test]
