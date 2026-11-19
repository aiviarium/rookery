//! The machine rookery creates for a host with a provisioner of a plugin:
//! what there is to create it with, what the chosen one requires, the
//! machine's size and the rest of its arguments, and the problems that stop
//! it. The machine is the host's resource of the machine kind. The set-up
//! flow asks for these; the server runs the jobs.

use crate::setup::Problem;
use rookery_api::{NewResource, ProvisionerInfo, Resource};
use std::collections::BTreeMap;

/// What is known of a host's machine and of the provisioners, and the form
/// of the one chosen to create the machine with.
#[derive(Default)]
pub struct Machine {
    /// They were asked for.
    pub asked: bool,
    pub provisioners: Option<Result<Vec<ProvisionerInfo>, String>>,
    /// The machine rookery created for the host, the resource of the machine
    /// kind whose host it is; `None` inside when it has none.
    pub instance: Option<Result<Option<Resource>, String>>,
    /// The chosen provisioner, by its name.
    pub chosen: Option<String>,
    /// What its arguments are given, by name.
    pub values: BTreeMap<String, String>,
}

fn label(argument: &rookery_api::Argument) -> &str {
    match argument.label.as_str() {
        "" => &argument.name,
        label => label,
    }
}

/// A count of CPUs: a positive number, such as 4 or 1.5.
pub fn valid_count(value: &str) -> bool {
    value
        .parse::<f64>()
        .is_ok_and(|count| count > 0.0 && count.is_finite())
        && value.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
}

/// A size with its unit, such as 8GiB, 512MiB or 4g.
pub fn valid_size(value: &str) -> bool {
    let split = value
        .find(|ch: char| !ch.is_ascii_digit() && ch != '.')
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let units = [
        "b", "k", "kb", "kib", "m", "mb", "mib", "g", "gb", "gib", "t", "tb", "tib",
    ];
    valid_count(number) && units.contains(&unit.to_ascii_lowercase().as_str())
}

/// Why a resource's value would not do, by what its name says it is.
fn resource_problem(argument: &rookery_api::Argument, value: &str) -> Option<String> {
    let name = argument.name.to_ascii_lowercase();
    let sized = ["memory", "mem", "ram", "disk", "size", "storage", "swap"];
    if name.contains("cpu") {
        (!valid_count(value))
            .then(|| format!("{} must be a positive number, such as 4", label(argument)))
    } else if sized.iter().any(|word| name.contains(word)) {
        (!valid_size(value)).then(|| {
            format!(
                "{} must be a size with its unit, such as 8GiB, 512MiB or 4g",
                label(argument)
            )
        })
    } else {
        None
    }
}

impl Machine {
    /// Forget what was known, before asking again.
    pub fn asking(&mut self) {
        self.asked = true;
        self.provisioners = None;
        self.instance = None;
    }

    /// Take in the provisioners there are. The first is chosen until
    /// another is, as a choice of a field has a value from the start.
    pub fn listed(&mut self, provisioners: Result<Vec<ProvisionerInfo>, String>) {
        self.provisioners = Some(provisioners);
        // One that is gone takes its form with it.
        if self.provisioner().is_none() {
            self.chosen = None;
            self.values.clear();
            self.choose(0);
        }
    }

    pub fn found(&mut self, instance: Result<Option<Resource>, String>) {
        // A machine there is was made by its provisioner, which is the chosen one then.
        if let Ok(Some(made)) = &instance {
            let id = made.provisioner.clone();
            self.choose_id(&id);
        }
        // One planned, as for a duplicate, is the form to create it with.
        if let Ok(Some(planned)) = &instance
            && planned.state == rookery_api::PLANNED
        {
            if self.chosen.as_deref() == Some(planned.provisioner.as_str()) {
                self.values.extend(planned.arguments.clone());
            }
            self.instance = Some(Ok(None));
            return;
        }
        self.instance = Some(instance);
    }

    fn all(&self) -> &[ProvisionerInfo] {
        match &self.provisioners {
            Some(Ok(all)) => all,
            _ => &[],
        }
    }

    /// The provisioner chosen to create the machine with.
    pub fn provisioner(&self) -> Option<&ProvisionerInfo> {
        let chosen = self.chosen.as_ref()?;
        self.all().iter().find(|known| known.named() == chosen)
    }

    /// Choose a provisioner: its form starts from the provisioner's kept
    /// settings, such as its server, and the defaults of the rest.
    pub fn choose(&mut self, index: usize) {
        let Some(provisioner) = self.all().get(index) else {
            return;
        };
        if self.chosen.as_ref() == Some(&provisioner.named().to_owned()) {
            return;
        }
        let values = provisioner
            .arguments
            .iter()
            .map(|argument| {
                let value = match provisioner.settings.get(&argument.name) {
                    Some(kept) if !kept.is_empty() => kept.clone(),
                    _ => argument.default.clone(),
                };
                (argument.name.clone(), value)
            })
            .collect();
        self.chosen = Some(provisioner.named().to_owned());
        self.values = values;
    }

    /// The provisioners to choose from, as (label, name), the way a
    /// choice field offers its values.
    pub fn choices(&self) -> Vec<(String, String)> {
        self.all()
            .iter()
            .map(|known| {
                // Two provisioners of one type differ by their names alone.
                let label = match known.kind.is_empty() || known.kind == known.named() {
                    true => known.label.clone(),
                    false => format!("{} · {}", known.named(), known.label),
                };
                (label, known.named().to_owned())
            })
            .collect()
    }

    /// Choose the provisioner with this name.
    pub fn choose_id(&mut self, id: &str) {
        if let Some(index) = self.all().iter().position(|known| known.named() == id) {
            self.choose(index);
        }
    }

    /// Whether the answers have come, so what they say can be judged.
    fn answered(&self) -> bool {
        self.provisioners.is_some() && self.instance.is_some()
    }

    /// Why creating with the chosen provisioner would be refused, each on
    /// the part of the set-up it is about. Nothing before anything was asked.
    pub fn problems(&self) -> Vec<Problem> {
        if !self.asked || !self.answered() || self.exists() {
            return vec![];
        }
        let mut problems = vec![];
        if let Some(Err(error)) = &self.instance {
            problems.push(Problem::new(crate::fields::Step::Run, error.clone()));
        }
        match &self.provisioners {
            Some(Err(error)) => {
                problems.push(Problem::new(crate::fields::Step::Install, error.clone()))
            }
            Some(Ok(all)) if all.is_empty() => problems.push(Problem::new(
                crate::fields::Step::Install,
                "No provisioner is registered. Add one with rookery provisioner add.",
            )),
            _ => {}
        }
        let Some(provisioner) = self.provisioner() else {
            if !self.all().is_empty() {
                problems.push(Problem::new(
                    crate::fields::Step::Install,
                    "Choose a provisioner",
                ));
            }
            return problems;
        };
        if !provisioner.missing_secrets.is_empty() {
            problems.push(Problem::new(
                crate::fields::Step::Install,
                format!(
                    "Needs the secret{} {}: rookery secret set NAME",
                    if provisioner.missing_secrets.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    provisioner.missing_secrets.join(", ")
                ),
            ));
        }
        for argument in &provisioner.arguments {
            let value = self
                .values
                .get(&argument.name)
                .map_or("", |value| value.trim());
            let step = Machine::step_of(argument);
            if value.is_empty() {
                if argument.required {
                    problems.push(Problem::new(
                        step,
                        format!("{} is required", label(argument)),
                    ));
                }
                continue;
            }
            if argument.is_resource()
                && let Some(problem) = resource_problem(argument, value)
            {
                problems.push(Problem::new(step, problem));
            }
        }
        problems
    }

    /// The part of the set-up an argument of the chosen provisioner is in:
    /// a required one
    /// has to be chosen, the rest have defaults.
    pub fn step_of(argument: &rookery_api::Argument) -> crate::fields::Step {
        match argument.required {
            true => crate::fields::Step::Install,
            false => crate::fields::Step::Settings,
        }
    }

    /// What creating the machine asks for, once nothing is in the way.
    pub fn request(&self) -> Result<NewResource, String> {
        let provisioner = self.provisioner().ok_or("Choose a provisioner first")?;
        if let Some(problem) = self.problems().into_iter().next() {
            return Err(problem.text);
        }
        let arguments = provisioner
            .arguments
            .iter()
            .map(|argument| {
                let value = self.values.get(&argument.name).cloned().unwrap_or_default();
                (argument.name.clone(), value.trim().to_owned())
            })
            .collect();
        Ok(NewResource {
            provisioner: provisioner.named().to_owned(),
            kind: "machine".into(),
            arguments,
            ..NewResource::default()
        })
    }

    /// Whether rookery created a machine for the host.
    pub fn exists(&self) -> bool {
        matches!(self.instance, Some(Ok(Some(_))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fields::Step;
    use rookery_api::Argument;

    fn provisioner(missing: &[&str]) -> ProvisionerInfo {
        ProvisionerInfo {
            id: "lab/vm".into(),
            label: "Lab machine".into(),
            description: "A machine in the lab.".into(),
            setup: "seed".into(),
            verbs: vec!["start".into(), "stop".into()],
            secrets: vec!["lab_token".into()],
            missing_secrets: missing.iter().map(|name| (*name).to_owned()).collect(),
            arguments: vec![
                Argument {
                    name: "server".into(),
                    label: "Lab server".into(),
                    required: true,
                    ..Default::default()
                },
                Argument {
                    name: "cpus".into(),
                    label: "vCPUs".into(),
                    default: "4".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    fn without_machine(provisioners: Vec<ProvisionerInfo>) -> Machine {
        let mut machine = Machine::default();
        machine.asking();
        machine.listed(Ok(provisioners));
        machine.found(Ok(None));
        machine
    }

    fn texts(problems: Vec<Problem>) -> Vec<(Step, String)> {
        problems
            .into_iter()
            .map(|problem| (problem.step, problem.text))
            .collect()
    }

    /// A provisioner that sizes its machine, as the Incus plugin's does.
    fn sized(id: &str, label: &str, resources: &[&str]) -> ProvisionerInfo {
        let mut arguments = vec![Argument {
            name: "server".into(),
            label: "Server".into(),
            required: true,
            ..Default::default()
        }];
        for name in resources {
            arguments.push(Argument {
                name: (*name).into(),
                label: (*name).into(),
                default: "1".into(),
                kind: Argument::RESOURCE.into(),
                ..Default::default()
            });
        }
        arguments.push(Argument {
            name: "image".into(),
            label: "Image".into(),
            default: "ubuntu".into(),
            ..Default::default()
        });
        ProvisionerInfo {
            id: id.into(),
            label: label.into(),
            setup: "seed".into(),
            arguments,
            ..Default::default()
        }
    }

    #[test]
    fn nothing_is_a_problem_before_it_was_asked() {
        let machine = Machine::default();
        assert!(machine.problems().is_empty());
        let none = without_machine(vec![]);
        assert_eq!(
            texts(none.problems()),
            [(
                Step::Install,
                "No provisioner is registered. Add one with rookery provisioner add.".into()
            )]
        );
    }

    #[test]
    fn a_kept_setting_of_the_provisioner_fills_what_it_requires() {
        let mut kept = provisioner(&[]);
        kept.settings =
            BTreeMap::from([("server".to_owned(), "https://192.0.2.10:8443".to_owned())]);
        let machine = without_machine(vec![kept]);
        assert!(machine.problems().is_empty());
        let request = machine.request().unwrap();
        assert_eq!(request.arguments["server"], "https://192.0.2.10:8443");
        assert_eq!(request.arguments["cpus"], "4");
    }

    #[test]
    fn a_required_argument_is_a_problem_of_the_first_step() {
        let machine = without_machine(vec![provisioner(&[])]);
        assert_eq!(
            texts(machine.problems()),
            [(Step::Install, "Lab server is required".into())]
        );
        assert!(machine.request().is_err());
    }

    #[test]
    fn the_first_provisioner_is_chosen_and_the_others_by_their_names() {
        let mut machine = without_machine(vec![
            sized("incus/vm", "Incus virtual machine", &["cpus", "memory"]),
            sized("docker/container", "Docker container", &["cpus", "memory"]),
        ]);
        assert_eq!(machine.chosen.as_deref(), Some("incus/vm"));
        assert_eq!(
            machine.choices(),
            [
                ("Incus virtual machine".to_owned(), "incus/vm".to_owned()),
                ("Docker container".to_owned(), "docker/container".to_owned())
            ]
        );
        // One chosen by its id comes with its own form.
        machine.choose_id("docker/container");
        assert_eq!(machine.chosen.as_deref(), Some("docker/container"));
        machine.choose(0);
        assert_eq!(machine.chosen.as_deref(), Some("incus/vm"));
        // A name no provisioner has changes nothing.
        machine.choose_id("lab/vm");
        assert_eq!(machine.chosen.as_deref(), Some("incus/vm"));
    }

    #[test]
    fn a_bad_resource_is_a_problem_of_the_settings_and_blocks_creating() {
        let mut machine = without_machine(vec![sized(
            "incus/vm",
            "Incus virtual machine",
            &["cpus", "memory", "disk"],
        )]);
        machine
            .values
            .insert("server".into(), "https://192.0.2.10:8443".into());
        for (name, value) in [("cpus", "4"), ("memory", "8GiB"), ("disk", "60g")] {
            machine.values.insert(name.into(), value.into());
        }
        assert!(machine.problems().is_empty());
        machine.values.insert("cpus".into(), "four".into());
        machine.values.insert("memory".into(), "8".into());
        assert_eq!(
            texts(machine.problems()),
            [
                (
                    Step::Settings,
                    "cpus must be a positive number, such as 4".into()
                ),
                (
                    Step::Settings,
                    "memory must be a size with its unit, such as 8GiB, 512MiB or 4g".into()
                ),
            ]
        );
        assert!(machine.request().unwrap_err().contains("cpus"));
        // An optional one left empty takes the provisioner's own default.
        machine.values.insert("cpus".into(), "".into());
        machine.values.insert("memory".into(), "512MiB".into());
        assert!(machine.problems().is_empty());
        assert!(valid_count("1.5") && !valid_count("0") && !valid_count("-2"));
        assert!(valid_size("4g") && valid_size("512MiB") && !valid_size("GiB"));
    }

    #[test]
    fn missing_secrets_are_a_problem_of_the_first_step() {
        let machine = without_machine(vec![provisioner(&["lab_token"])]);
        let problems = texts(machine.problems());
        assert_eq!(problems[0].0, Step::Install);
        assert!(
            problems[0]
                .1
                .contains("Needs the secret lab_token: rookery secret set NAME")
        );
        assert!(machine.request().is_err());
    }

    #[test]
    fn a_planned_machine_is_the_form_to_create_it_with() {
        let mut machine = Machine::default();
        machine.asking();
        machine.listed(Ok(vec![provisioner(&[])]));
        machine.found(Ok(Some(Resource {
            provisioner: "lab/vm".into(),
            id: "r-lab".into(),
            state: "planned".into(),
            arguments: [("cpus".to_owned(), "6".to_owned())].into(),
            ..Default::default()
        })));
        assert!(!machine.exists());
        assert_eq!(machine.chosen.as_deref(), Some("lab/vm"));
        assert_eq!(machine.values["cpus"], "6");
    }

    #[test]
    fn a_machine_there_is_has_nothing_to_choose() {
        let mut machine = Machine::default();
        machine.asking();
        machine.listed(Ok(vec![provisioner(&[])]));
        machine.found(Ok(Some(Resource {
            provisioner: "lab/vm".into(),
            id: "r-studio".into(),
            external_id: "studio".into(),
            state: "running".into(),
            address: "192.0.2.20".into(),
            verbs: vec!["start".into(), "stop".into()],
            ..Default::default()
        })));
        assert!(machine.exists());
        assert!(machine.problems().is_empty());
    }

    #[test]
    fn an_error_asking_for_the_machine_is_in_the_way_of_creating_it() {
        let mut machine = Machine::default();
        machine.asking();
        machine.listed(Ok(vec![provisioner(&[])]));
        machine.found(Err("The rookery server did not answer".into()));
        assert!(
            texts(machine.problems())
                .contains(&(Step::Run, "The rookery server did not answer".into()))
        );
    }
}
